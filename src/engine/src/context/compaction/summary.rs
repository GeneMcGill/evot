//! Summary chain — the single HOW of compaction, shared by the automatic
//! agent-loop path and the manual `/compact` orchestrator:
//!
//! ```text
//! override text → provider-native remote → LLM text summary → emergency
//! ```
//!
//! Each stage that fails records why and falls through to the next; user
//! cancellation aborts the chain without producing a summary.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::config::truncate_summary;
use super::config::CompactionConfig;
use super::emergency;
use super::remote;
use super::summarizer::mode::SummarizerContext;
use super::summarizer::serialize;
use super::summarizer::types::SummarizerError;
use super::summarizer::types::SummarizerInput;
use super::types::bounded_fallback_reason;
use super::types::notify_compaction_phase;
use super::types::CompactionMethod;
use super::types::CompactionObserver;
use super::types::CompactionPhase;
use super::types::CompactionState;
use super::types::CompactionUpdate;
use super::types::FileOps;
use crate::types::AgentMessage;

/// Provider contexts used by independent summary stages.
///
/// Remote compaction must use the active request's model/transport because its
/// opaque state is replayed there. Local LLM summarization may use a dedicated
/// compaction model, with an optional fallback model.
#[derive(Clone, Copy, Default)]
pub struct SummaryContexts<'a> {
    pub remote: Option<&'a SummarizerContext>,
    pub llm: Option<&'a SummarizerContext>,
    pub llm_fallback: Option<&'a SummarizerContext>,
}

impl<'a> SummaryContexts<'a> {
    pub fn same(ctx: Option<&'a SummarizerContext>) -> Self {
        Self {
            remote: ctx,
            llm: ctx,
            llm_fallback: None,
        }
    }

    pub fn separate(
        remote: Option<&'a SummarizerContext>,
        llm: Option<&'a SummarizerContext>,
        llm_fallback: Option<&'a SummarizerContext>,
    ) -> Self {
        Self {
            remote,
            llm,
            llm_fallback,
        }
    }

    /// Tokens occupied by the active request shape. Dedicated summary models
    /// are intentionally ignored because retained history is replayed to the
    /// active model, represented by `remote`.
    pub fn request_overhead_tokens(&self) -> usize {
        let Some(ctx) = self.remote else {
            return 0;
        };
        let system = crate::context::estimate_tokens(&ctx.system_prompt);
        let tools = serde_json::to_string(&ctx.tools)
            .map(|json| crate::context::estimate_tokens(&json))
            .unwrap_or_else(|_| {
                ctx.tools
                    .iter()
                    .map(|tool| {
                        crate::context::estimate_tokens(&tool.name)
                            + crate::context::estimate_tokens(&tool.description)
                            + crate::context::estimate_tokens(&tool.parameters.to_string())
                    })
                    .sum()
            });
        system.saturating_add(tools)
    }
}

/// How the chain treats the LLM stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmPolicy {
    /// Use the LLM when a context is available; an attempted LLM failure aborts
    /// compaction. Without a context, use the deterministic summary.
    Required,
    /// Use the LLM; if it fails or times out, use the deterministic emergency
    /// summary instead (manual `/compact` path).
    PreferLlm,
    /// Skip the LLM entirely — overflow recovery must not issue a second
    /// request to the provider that just rejected the oversized payload.
    Skip,
}

/// What to summarize. Slices reference the caller's (post-reclaim) messages.
pub struct SummaryRequest<'a> {
    /// Summarize zone (excludes the split-turn prefix).
    pub evicted: &'a [AgentMessage],
    /// Split-turn prefix, summarized separately.
    pub turn_prefix: Option<&'a [AgentMessage]>,
    /// Cross-compaction state: previous summary, cumulative file ops.
    pub prev_state: Option<&'a CompactionState>,
    /// Optional user guidance for the summary.
    pub custom_instructions: Option<&'a str>,
    /// Planner-extracted file ops, merged into the serialized input.
    pub file_ops: FileOps,
    /// Pre-supplied summary text: skips remote and LLM stages entirely.
    pub override_text: Option<String>,
}

/// Runtime controls for one chain run.
pub struct SummaryOptions {
    pub llm_policy: LlmPolicy,
    /// Wall-clock bound per stage (remote / LLM). `None` = unbounded.
    pub timeout: Option<Duration>,
    pub observer: Option<CompactionObserver>,
    pub cancel: CancellationToken,
}

/// Result of the chain.
pub enum SummaryOutcome {
    Ready(SummaryResult),
    /// LLM failed under `LlmPolicy::Required` — compaction must not proceed.
    Aborted,
    /// User cancelled — compaction must not proceed and must not fall back.
    Cancelled,
}

pub struct SummaryResult {
    /// Bounded readable summary. For remote compaction this is the
    /// deterministic portability text shown to non-Responses providers.
    pub text: String,
    /// Provider-native compaction item, when the remote stage succeeded.
    pub remote: Option<remote::RemoteCompaction>,
    pub method: CompactionMethod,
    /// Why remote compaction was unavailable or failed before local fallback.
    pub fallback_reason: Option<String>,
    /// True when the deterministic summary replaced a requested LLM summary.
    pub used_fallback: bool,
}

/// Run the chain and produce a summary for the evicted zone.
pub async fn summarize(
    request: SummaryRequest<'_>,
    contexts: SummaryContexts<'_>,
    config: &CompactionConfig,
    mut options: SummaryOptions,
) -> SummaryOutcome {
    let input = prepare_input(&request, config, serialize::SerializationOptions::default());

    // Stage 0: explicit override — stays local, keeps the split-turn context.
    if let Some(text) = request.override_text {
        notify_compaction_phase(&options.observer, CompactionPhase::Local);
        return SummaryOutcome::Ready(SummaryResult {
            text: finalize(
                with_turn_context(text, input.turn_prefix.as_deref()),
                config,
            ),
            remote: None,
            method: CompactionMethod::Local,
            fallback_reason: None,
            used_fallback: false,
        });
    }

    // Stage 1: provider-native remote compaction.
    let mut method = CompactionMethod::Local;
    let mut fallback_reason = None;
    match run_remote_stage(&request, contexts.remote, &options).await {
        RemoteStage::Ready(compaction) => {
            // The opaque item is authoritative; the deterministic text rides
            // along as the portability fallback for non-Responses providers.
            return SummaryOutcome::Ready(SummaryResult {
                text: finalize(emergency::summarize(&input).summary, config),
                remote: Some(compaction),
                method: CompactionMethod::Remote,
                fallback_reason: None,
                used_fallback: false,
            });
        }
        RemoteStage::Cancelled => return SummaryOutcome::Cancelled,
        RemoteStage::Failed(reason) => {
            tracing::warn!("remote compaction failed, falling back to local: {reason}");
            method = CompactionMethod::RemoteFailedLocal;
            fallback_reason = Some(bounded_fallback_reason(&reason));
            notify_compaction_phase(&options.observer, CompactionPhase::LocalFallback);
            // Streaming activity refines this phase, rather than making older
            // clients think we left the remote-failure fallback path.
            options.observer = options.observer.take().map(|observer| {
                Arc::new(move |mut update: CompactionUpdate| {
                    if update.phase == CompactionPhase::Local {
                        update.phase = CompactionPhase::LocalFallback;
                    }
                    observer(update);
                }) as CompactionObserver
            });
        }
        RemoteStage::Unsupported => {
            notify_compaction_phase(&options.observer, CompactionPhase::Local);
        }
    }

    // Stage 2: LLM text summary.
    let mut used_fallback = false;
    if options.llm_policy != LlmPolicy::Skip {
        if let Some(ctx) = contexts.llm {
            match run_llm_with_degradation(
                &request,
                input.clone(),
                ctx,
                contexts.llm_fallback,
                config,
                &options,
            )
            .await
            {
                LlmStage::Ready(text) => {
                    return SummaryOutcome::Ready(SummaryResult {
                        text: finalize(text, config),
                        remote: None,
                        method,
                        fallback_reason,
                        used_fallback: false,
                    });
                }
                LlmStage::Cancelled => return SummaryOutcome::Cancelled,
                LlmStage::Failed(_) => match options.llm_policy {
                    LlmPolicy::Required => return SummaryOutcome::Aborted,
                    LlmPolicy::PreferLlm => used_fallback = true,
                    LlmPolicy::Skip => {}
                },
            }
        } else {
            used_fallback = true;
        }
    }

    // Stage 3: deterministic emergency summary — never issues a model request.
    if options.cancel.is_cancelled() {
        return SummaryOutcome::Cancelled;
    }
    SummaryOutcome::Ready(SummaryResult {
        text: finalize(emergency::summarize(&input).summary, config),
        remote: None,
        method,
        fallback_reason,
        used_fallback,
    })
}

// ---------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------

enum RemoteStage {
    Ready(remote::RemoteCompaction),
    Failed(String),
    Cancelled,
    Unsupported,
}

async fn run_remote_stage(
    request: &SummaryRequest<'_>,
    ctx: Option<&SummarizerContext>,
    options: &SummaryOptions,
) -> RemoteStage {
    let Some(ctx) = ctx.filter(|ctx| remote::supports(ctx)) else {
        return RemoteStage::Unsupported;
    };
    notify_compaction_phase(&options.observer, CompactionPhase::Remote);

    // A prior local summary transitions into provider-native state exactly once.
    let previous_local_summary = request.prev_state.and_then(|state| {
        state
            .context_summary_message
            .as_ref()
            .and(state.last_summary.as_deref())
    });
    let mut remote_input =
        remote::with_previous_local_summary(request.evicted, previous_local_summary);
    if let Some(prefix) = request.turn_prefix {
        remote_input.extend_from_slice(prefix);
    }

    let cancel = stage_cancel(options);
    let result = match options.timeout {
        Some(timeout) => {
            match tokio::time::timeout(timeout, remote::compact(ctx, &remote_input, cancel.clone()))
                .await
            {
                Ok(result) => result,
                Err(_) => {
                    cancel.cancel();
                    Err(remote::RemoteError::Failed(timeout_reason(timeout)))
                }
            }
        }
        None => remote::compact(ctx, &remote_input, cancel).await,
    };

    match result {
        Ok(compaction) => RemoteStage::Ready(compaction),
        Err(remote::RemoteError::Cancelled) => RemoteStage::Cancelled,
        Err(remote::RemoteError::Failed(_)) if options.cancel.is_cancelled() => {
            RemoteStage::Cancelled
        }
        Err(remote::RemoteError::Failed(reason)) => RemoteStage::Failed(reason),
    }
}

enum LlmStage {
    Ready(String),
    Failed(String),
    Cancelled,
}

/// Primary model first; any failure hands the same input to the fallback
/// model (typically the active model after a switch, when the previous one is
/// gone). A context overflow on the last model tried gets one more attempt
/// with the tool transcript abbreviated.
async fn run_llm_with_degradation(
    request: &SummaryRequest<'_>,
    input: SummarizerInput,
    primary: &SummarizerContext,
    fallback: Option<&SummarizerContext>,
    config: &CompactionConfig,
    options: &SummaryOptions,
) -> LlmStage {
    let mut ctx = primary;
    let mut reason = match run_llm_stage(input.clone(), ctx, config, options).await {
        LlmStage::Failed(reason) => reason,
        done => return done,
    };
    if let Some(fallback) = fallback.filter(|fallback| fallback.model != primary.model) {
        tracing::warn!(
            model = %primary.model,
            fallback_model = %fallback.model,
            error = %reason,
            "compaction summary failed on primary model; trying fallback model"
        );
        ctx = fallback;
        reason = match run_llm_stage(input, ctx, config, options).await {
            LlmStage::Failed(reason) => reason,
            done => return done,
        };
    }
    if !crate::provider::error::is_context_overflow_message(&reason) {
        return LlmStage::Failed(reason);
    }
    tracing::warn!(
        model = %ctx.model,
        "compaction summary exceeds model context; abbreviating tool transcript"
    );
    let abbreviated = prepare_input(request, config, serialize::SerializationOptions {
        abbreviate_tools: true,
    });
    run_llm_stage(abbreviated, ctx, config, options).await
}

async fn run_llm_stage(
    input: SummarizerInput,
    ctx: &SummarizerContext,
    config: &CompactionConfig,
    options: &SummaryOptions,
) -> LlmStage {
    let cancel = stage_cancel(options);
    let summarize = config.summarizer_mode.summarize_with_observer(
        input,
        Some(ctx),
        config.summary_reserve_tokens() as u32,
        cancel.clone(),
        options.observer.clone(),
    );
    let result = match options.timeout {
        Some(timeout) => match tokio::time::timeout(timeout, summarize).await {
            Ok(result) => result,
            Err(_) => {
                cancel.cancel();
                tracing::warn!(
                    stage = "compact",
                    status = "summary_timeout",
                    "{}",
                    timeout_reason(timeout)
                );
                Err(SummarizerError::Failed(timeout_reason(timeout)))
            }
        },
        None => summarize.await,
    };

    if options.cancel.is_cancelled() {
        return LlmStage::Cancelled;
    }
    match result {
        Ok(output) if !output.summary.trim().is_empty() => LlmStage::Ready(output.summary),
        Ok(_) => LlmStage::Failed("summarizer returned empty text".into()),
        Err(SummarizerError::Cancelled) => LlmStage::Cancelled,
        Err(SummarizerError::Failed(reason)) => LlmStage::Failed(reason),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn prepare_input(
    request: &SummaryRequest<'_>,
    config: &CompactionConfig,
    serialization: serialize::SerializationOptions,
) -> SummarizerInput {
    let mut input = serialize::prepare_input_with_options(
        request.evicted,
        request.turn_prefix,
        request.prev_state,
        config,
        serialization,
    );
    input.custom_instructions = request
        .custom_instructions
        .filter(|text| !text.trim().is_empty())
        .map(|text| truncate_summary(text, config.summary_max_bytes()));
    merge_file_ops(&mut input.file_ops, &request.file_ops);
    input
}

fn merge_file_ops(target: &mut FileOps, extra: &FileOps) {
    target.read.extend(extra.read.iter().cloned());
    target.written.extend(extra.written.iter().cloned());
    target.edited.extend(extra.edited.iter().cloned());
}

/// Bound the final summary text.
fn finalize(text: String, config: &CompactionConfig) -> String {
    truncate_summary(&text, config.summary_max_bytes())
}

/// Preserve the evicted split-turn prefix alongside an override summary.
fn with_turn_context(mut text: String, turn_prefix: Option<&str>) -> String {
    if let Some(prefix) = turn_prefix.filter(|prefix| !prefix.trim().is_empty()) {
        text.push_str("\n\n---\n\n**Turn Context (split turn):**\n\n");
        text.push_str(prefix);
    }
    text
}

/// Stage token: with a timeout the stage gets a child token so expiry can
/// abort it without cancelling the whole compaction; without one, the caller's
/// token is passed straight through (providers may observe it directly).
fn stage_cancel(options: &SummaryOptions) -> CancellationToken {
    match options.timeout {
        Some(_) => options.cancel.child_token(),
        None => options.cancel.clone(),
    }
}

fn timeout_reason(timeout: Duration) -> String {
    format!(
        "summary stage timed out after {} seconds",
        timeout.as_secs()
    )
}
