//! Slash-command parsing for `/clear`, `/compact`, etc.
//!
//! Commands are core domain operations executed by the agent; channels and the
//! gateway only forward user text here.

// ---------------------------------------------------------------------------
// Command — parsed gateway commands
// ---------------------------------------------------------------------------

pub enum Command {
    Clear,
    Compact {
        custom_instructions: Option<String>,
    },
    /// Hidden `/_dump` — emit current system prompt + tools as JSON.
    /// Optional argument is an output path. When None, the agent picks a
    /// timestamped default under `~/.evotai/dumps/`.
    Dump {
        target: Option<String>,
    },
    /// `/clip all` — distill durable knowledge from the current conversation
    /// into the memory vault through a normal LLM turn. Bare `/clip` is a TUI
    /// command that saves the latest assistant reply locally.
    ClipSession,
    /// `/sessions <request>` — find past sessions by meaning. The model reads
    /// the time range and topic from the request itself and scans the archive
    /// through the run-scoped `session_search` tool. Bare `/sessions` is a TUI
    /// command (the resume selector).
    SessionSearch(crate::search::SessionSearch),
    UsageError(String),
}

/// Wrap a host-expanded command so readers can tell the user's line from the
/// instructions the host added. The model reads the whole message; replays,
/// titles, search digests and shared pages show `input` and fold the rest.
/// Mirrors `cli/src/session/command-envelope.ts`; keep both shapes identical.
pub fn command_prompt(name: &str, input: &str, instructions: &str) -> String {
    let name = if !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        name
    } else {
        "command"
    };
    let input = input
        .trim()
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;");
    format!(
        "<command name=\"{name}\" input=\"{input}\">\n{}\n</command>",
        instructions.trim()
    )
}

/// A parsed command envelope; `None` for ordinary user text.
pub struct CommandEnvelope<'a> {
    pub name: &'a str,
    pub input: String,
    pub instructions: &'a str,
    /// User text that followed the envelope, if any.
    pub text: Option<&'a str>,
}

pub fn parse_command_prompt(text: &str) -> Option<CommandEnvelope<'_>> {
    let rest = text.strip_prefix("<command name=\"")?;
    let name_end = rest.find('"')?;
    let name = &rest[..name_end];
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return None;
    }
    let rest = rest[name_end..].strip_prefix("\" input=\"")?;
    let input_end = rest.find('"')?;
    let raw_input = &rest[..input_end];
    if raw_input.contains('\n') {
        return None;
    }
    let body = rest[input_end..].strip_prefix("\">\n")?;
    let close = body.find("\n</command>")?;
    let instructions = &body[..close];
    let tail = &body[close + "\n</command>".len()..];
    let text = match tail {
        "" => None,
        _ => {
            let text = tail.strip_prefix("\n\n")?.trim();
            (!text.is_empty()).then_some(text)
        }
    };
    let input = raw_input
        .replace("&#10;", "\n")
        .replace("&#13;", "\r")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&");
    Some(CommandEnvelope {
        name,
        input,
        instructions,
        text,
    })
}

/// A background-task completion the host hands the model as user text. The
/// whole message must be the envelope; prose quoting the tag stays user text.
pub fn is_task_notification(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("<task-notification>\n<task-id>")
        && trimmed.ends_with("</task-notification>")
        && trimmed.matches("<task-notification>").count() == 1
}

/// What a stored user turn says in the user's own words: the typed command
/// line for an expanded command, the text itself otherwise. Host-initiated
/// turns (loop wakeups, task notifications) carry nothing the user said and
/// yield `None`.
pub fn user_facing_text(text: &str) -> Option<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    if is_task_notification(text) {
        return None;
    }
    match parse_command_prompt(text) {
        None => Some(Cow::Borrowed(text)),
        Some(envelope) if envelope.name == "loop-wakeup" => None,
        Some(envelope) => {
            if envelope.input.is_empty() {
                envelope.text.map(Cow::Borrowed)
            } else {
                Some(Cow::Owned(envelope.input))
            }
        }
    }
}

/// Build the prompt `/clip all` expands into. The command injects the memory
/// workflow instead of asking the model to discover it from the skill index.
pub fn clip_session_prompt(memory_instructions: &str) -> String {
    format!(
        "The `memory` skill is already loaded for this command. Archive the durable knowledge \
         from this conversation into the memory vault now. Do not claim that the memory skill \
         is unavailable. Follow the skill instructions below.\n\n\
         ---\n{memory_instructions}"
    )
}

/// Queue guard shared by product adapters. This mirrors CLI command-shape
/// recognition, not execution parsing: paths and a lone slash remain prompts.
pub fn is_queued_command(text: &str) -> bool {
    let token = text.split_whitespace().next().unwrap_or("");
    match token.strip_prefix('/') {
        Some(name) => {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        }
        None => false,
    }
}

pub fn parse_command(text: &str) -> Option<Command> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();

    if lower == "/clear" {
        return Some(Command::Clear);
    }
    if lower == "/compact" || lower.starts_with("/compact ") {
        let arg = trimmed
            .strip_prefix("/compact")
            .or_else(|| trimmed.strip_prefix("/COMPACT"))
            .map(str::trim)
            .unwrap_or("");
        return Some(Command::Compact {
            custom_instructions: (!arg.is_empty()).then(|| arg.to_string()),
        });
    }
    if lower == "/_dump" || lower.starts_with("/_dump ") {
        let arg = trimmed
            .strip_prefix("/_dump")
            .or_else(|| trimmed.strip_prefix("/_DUMP"))
            .map(str::trim)
            .unwrap_or("");
        let target = (!arg.is_empty()).then(|| arg.to_string());
        return Some(Command::Dump { target });
    }
    if lower == "/clip all" {
        return Some(Command::ClipSession);
    }
    if let Some(args) = trimmed.strip_prefix("/sessions ") {
        return Some(match crate::search::SessionSearch::parse(args) {
            Some(search) => Command::SessionSearch(search),
            None => Command::UsageError(
                "Usage: /sessions <what you are looking for, optionally with a time range>"
                    .to_string(),
            ),
        });
    }
    if lower == "/clip" || lower.starts_with("/clip ") {
        return Some(Command::UsageError(
            "/clip saves the last reply locally in the TUI; use `/clip all` to distill this session into memory".to_string(),
        ));
    }
    None
}
