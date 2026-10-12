//! Planner — decides WHAT to keep and what to summarize.
//!
//! This is the single planner for both compaction frontends: the automatic
//! agent-loop path (`plan_messages`) and the manual `/compact` orchestrator
//! (`plan_compaction`, sequence-numbered transcript entries). The output is a
//! declarative [`CompactionPlan`]; callers apply it via the executor or their
//! own persistence layer.

use std::ops::Range;

use super::types::FileOps;
use crate::context::tokens::message_tokens;
use crate::types::AgentMessage;
use crate::types::Content;
use crate::types::Message;

/// A message-bearing transcript entry with a stable sequence number.
#[derive(Debug, Clone)]
pub struct CompactEntry {
    pub seq: u64,
    pub message: AgentMessage,
}

/// Declarative plan for one compaction.
///
/// Ranges index into the planned slice. Nothing is pinned: the summary
/// represents all evicted history.
#[derive(Debug, Clone)]
pub struct CompactionPlan {
    /// Entries summarized into the compact summary.
    pub summarize: Range<usize>,
    /// Prefix of a split current turn, summarized separately so the retained
    /// suffix keeps its context.
    pub turn_prefix: Option<Range<usize>>,
    /// Index of the first entry retained verbatim.
    pub first_kept: usize,
    /// Sequence number of the first retained entry (persistence boundary).
    pub first_kept_seq: u64,
    pub split_turn: Option<SplitTurn>,
    pub tokens_before: usize,
    pub messages_before: usize,
    /// File operations extracted from everything being evicted.
    pub file_ops: FileOps,
}

/// The cut point fell inside a turn rather than at a user message boundary.
#[derive(Debug, Clone)]
pub struct SplitTurn {
    pub turn_start_seq: u64,
    pub cut_seq: u64,
}

/// Plan compaction over a sequence-numbered transcript.
///
/// `boundary_seq` is the previous compaction's first-kept sequence, if any.
/// Entries before it are already represented by the previous summary and are
/// not summarized again.
pub fn plan_compaction(
    entries: &[CompactEntry],
    boundary_seq: Option<u64>,
    keep_recent_tokens: usize,
) -> Option<CompactionPlan> {
    let messages: Vec<&AgentMessage> = entries.iter().map(|entry| &entry.message).collect();
    let boundary = boundary_seq
        .and_then(|seq| entries.iter().position(|entry| entry.seq >= seq))
        .unwrap_or(0);
    let core = plan_core(&messages, boundary, keep_recent_tokens, boundary)?;
    Some(assemble(core, &messages, |index| entries[index].seq))
}

/// Plan compaction over a plain message list (automatic agent-loop path).
/// Sequence numbers are synthesized as `index + 1`.
pub fn plan_messages(
    messages: &[AgentMessage],
    keep_recent_tokens: usize,
) -> Option<CompactionPlan> {
    plan_messages_from_boundary(messages, keep_recent_tokens, 0)
}

/// Plan automatic compaction while forcing the retained suffix to start no
/// earlier than `minimum_first_kept`. Overflow recovery uses this after the
/// normal pi-style cut cannot evict anything: the triggering request is already
/// known to be too large, so retaining the same oldest message would only
/// resend the rejected payload.
pub fn plan_messages_from_boundary(
    messages: &[AgentMessage],
    keep_recent_tokens: usize,
    minimum_first_kept: usize,
) -> Option<CompactionPlan> {
    let refs: Vec<&AgentMessage> = messages.iter().collect();
    let core = plan_core(&refs, 0, keep_recent_tokens, minimum_first_kept)?;
    Some(assemble(core, &refs, |index| index as u64 + 1))
}

// ---------------------------------------------------------------------------
// Core algorithm (index-based, shared by both frontends)
// ---------------------------------------------------------------------------

struct CorePlan {
    boundary: usize,
    summarize_end: usize,
    turn_prefix: Option<Range<usize>>,
    first_kept: usize,
    turn_start: Option<usize>,
}

fn plan_core(
    messages: &[&AgentMessage],
    boundary: usize,
    keep_recent_tokens: usize,
    minimum_first_kept: usize,
) -> Option<CorePlan> {
    if messages.is_empty() || boundary >= messages.len() {
        return None;
    }

    let first_kept = find_first_kept(messages, boundary, keep_recent_tokens)
        .max(minimum_first_kept.min(messages.len()));
    if first_kept <= boundary {
        return None; // Nothing to evict.
    }

    let turn_start = detect_split_turn(messages, boundary, first_kept);
    let summarize_end = turn_start.unwrap_or(first_kept);
    let turn_prefix = turn_start.map(|start| start..first_kept);

    if summarize_end <= boundary && turn_prefix.as_ref().is_none_or(|range| range.is_empty()) {
        return None;
    }

    Some(CorePlan {
        boundary,
        summarize_end,
        turn_prefix,
        first_kept,
        turn_start,
    })
}

fn assemble(
    core: CorePlan,
    messages: &[&AgentMessage],
    seq_of: impl Fn(usize) -> u64,
) -> CompactionPlan {
    let tokens_before = messages.iter().map(|message| message_tokens(message)).sum();
    // File ops cover the whole evicted span (summarize zone + turn prefix).
    let file_ops = extract_file_ops(&messages[core.boundary..core.first_kept]);

    CompactionPlan {
        summarize: core.boundary..core.summarize_end,
        turn_prefix: core.turn_prefix,
        first_kept: core.first_kept,
        first_kept_seq: if core.first_kept < messages.len() {
            seq_of(core.first_kept)
        } else {
            seq_of(messages.len() - 1).saturating_add(1)
        },
        split_turn: core.turn_start.map(|start| SplitTurn {
            turn_start_seq: seq_of(start),
            cut_seq: if core.first_kept < messages.len() {
                seq_of(core.first_kept)
            } else {
                seq_of(messages.len() - 1).saturating_add(1)
            },
        }),
        tokens_before,
        messages_before: messages.len(),
        file_ops,
    }
}

/// Walk backwards accumulating tokens until `keep_recent_tokens` is reached,
/// then snap forward to a valid cut point. The token budget is the only
/// retention condition.
fn find_first_kept(
    messages: &[&AgentMessage],
    boundary: usize,
    keep_recent_tokens: usize,
) -> usize {
    let Some(first_cut) = snap_forward_from(messages, boundary) else {
        return boundary;
    };
    let mut accumulated = 0usize;
    let mut candidate = first_cut; // All history fits: keep everything.

    for i in (boundary..messages.len()).rev() {
        let tokens = message_tokens(messages[i]);
        if tokens == 0 {
            continue;
        }
        accumulated += tokens;
        if accumulated >= keep_recent_tokens {
            // No valid cut at or after the message that crossed the budget
            // means it sits in the final tool batch. Keep that whole batch by
            // cutting at the assistant that issued it: retaining more than the
            // budget still evicts everything before, whereas falling back to
            // the first cut keeps the entire history and compacts nothing —
            // for as long as the newest tool result alone exceeds the budget.
            candidate = snap_forward_from(messages, i)
                .or_else(|| snap_backward_from(messages, boundary, i))
                .unwrap_or(first_cut);
            break;
        }
    }

    // Keep adjacent UI-only metadata with the retained tail.
    while candidate > boundary && matches!(messages[candidate - 1], AgentMessage::Extension(_)) {
        candidate -= 1;
    }
    candidate
}

/// Nearest valid cut point at or after `start`: a user or assistant message,
/// never a tool result.
fn snap_forward_from(messages: &[&AgentMessage], start: usize) -> Option<usize> {
    messages[start..]
        .iter()
        .position(|message| is_valid_cut_point(message))
        .map(|offset| start + offset)
}

/// Nearest valid cut point before `end`, not before `boundary`; `None` when
/// that would evict nothing.
fn snap_backward_from(messages: &[&AgentMessage], boundary: usize, end: usize) -> Option<usize> {
    (boundary + 1..end)
        .rev()
        .find(|&i| is_valid_cut_point(messages[i]))
}

fn is_valid_cut_point(msg: &AgentMessage) -> bool {
    matches!(
        msg,
        AgentMessage::Llm(Message::User { .. }) | AgentMessage::Llm(Message::Assistant { .. })
    )
}

/// When the first kept entry is not a user message, the cut splits a turn;
/// return the index of the user message that started it.
fn detect_split_turn(
    messages: &[&AgentMessage],
    boundary: usize,
    first_kept: usize,
) -> Option<usize> {
    if first_kept >= messages.len() || is_user(messages[first_kept]) {
        return None;
    }
    (boundary..first_kept).rev().find(|&i| is_user(messages[i]))
}

fn is_user(msg: &AgentMessage) -> bool {
    matches!(msg, AgentMessage::Llm(Message::User { .. }))
}

// ---------------------------------------------------------------------------
// File-op extraction
// ---------------------------------------------------------------------------

fn extract_file_ops(messages: &[&AgentMessage]) -> FileOps {
    let mut ops = FileOps::default();
    for message in messages {
        extract_file_ops_from_message(message, &mut ops);
    }
    ops
}

fn extract_file_ops_from_message(message: &AgentMessage, ops: &mut FileOps) {
    let AgentMessage::Llm(Message::Assistant { content, .. }) = message else {
        return;
    };
    for block in content {
        let Content::ToolCall {
            name, arguments, ..
        } = block
        else {
            continue;
        };
        let path = arguments
            .get("path")
            .or_else(|| arguments.get("file_path"))
            .or_else(|| arguments.get("filename"))
            .and_then(|v| v.as_str());
        let Some(path) = path else { continue };
        // `bash` is intentionally absent: its argument is a command, not a path.
        match name.as_str() {
            "edit" => {
                ops.edited.insert(path.to_string());
            }
            "write" => {
                ops.written.insert(path.to_string());
            }
            "read" | "grep" | "glob" => {
                ops.read.insert(path.to_string());
            }
            _ => {}
        }
    }
}
