use std::sync::Arc;
use std::time::Duration;

use evot_engine::provider::StreamConfig;
use evot_engine::provider::StreamProvider;
use evot_engine::Content;
use evot_engine::Message;
use evot_engine::StopReason;
use evot_engine::ThinkingLevel;
use serde::Deserialize;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::SuggestionEndpoint;

// Match Codex's safety deadline, including cold connection/gateway latency.
// This is not a display delay: completion is displayed immediately and input
// cancels instantly. Foreground interaction never waits for this request.
pub const SUGGESTION_TIMEOUT: Duration = Duration::from_secs(8);
const HISTORY_TOKENS: usize = 4_096;
const TOKEN_HEADROOM: usize = 1_024;
const OUTPUT_TOKENS: u32 = 32;
// Codex origin/fcoury/next-suggestion, context/next_prompt_suggestion.rs.
// Preserve the source prompt, replacing only the product name and appending
// the explicit user-language requirement. Silence is empty output, not NONE.
const INSTRUCTIONS: &str = r#"[SUGGESTION MODE: Suggest what the user might naturally type next into evot.]

FIRST: Look at the user's recent messages and original request.

Your job is to predict what THEY would type - not what you think they should do.

Think about the next logical step. If you can infer a goal or a target new functionality, offer the next logical step towards that apparent goal or functionality.

THE TEST: Would they think "I was just about to type that"?

NEVER SUGGEST:
- Evaluative ("looks good", "thanks")
- Questions ("what about...?")
- evot-voice ("Let me...", "I'll...", "Here's...")
- New ideas they didn't ask about
- Multiple sentences

Stay silent if the next step isn't obvious from what the user said.

Format: 2-12 words, match the user's style including capitalization, verbosity and others.

Reply with ONLY the suggestion, no quotes or explanation.

LANGUAGE IS MANDATORY: use the natural language of the most recent user message, even if the assistant or earlier messages use another language. Infer language from the user's prose, not code, paths, identifiers or product names. Never translate the suggestion into the assistant's language."#;

#[derive(Clone, Deserialize, Serialize)]
pub struct SuggestionMessage {
    pub role: String,
    pub text: String,
}

/// Single bounded request; deliberately not an agent run. No retries, tools,
/// compaction, transcript writes, main-model fallback or auth-refresh traffic.
pub async fn generate(
    messages: Vec<SuggestionMessage>,
    cancel: CancellationToken,
) -> Option<String> {
    let started = std::time::Instant::now();
    let work = async {
        let Some(endpoint) = super::endpoint::current().await else {
            tracing::debug!(target: "evot::suggestion", outcome = "not_published");
            return None;
        };
        let provider: Arc<dyn StreamProvider> = match endpoint.parsed_protocol()? {
            crate::conf::Protocol::Anthropic => Arc::new(evot_engine::provider::AnthropicProvider),
            crate::conf::Protocol::OpenAi => Arc::new(evot_engine::provider::OpenAiCompatProvider),
            crate::conf::Protocol::OpenAiResponses => {
                Arc::new(evot_engine::provider::OpenAiResponsesProvider)
            }
        };
        sample(&endpoint, &messages, provider, cancel.clone()).await
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        result = tokio::time::timeout(SUGGESTION_TIMEOUT, work) => match result {
            Ok(result) => result,
            Err(_) => {
                tracing::info!(target: "evot::suggestion", outcome = "timeout", elapsed_ms = started.elapsed().as_millis());
                None
            }
        },
    }
}

/// A contiguous recent suffix, independently bounded from callers. Like
/// Codex, skip sampling if the prompt cannot leave 1024 tokens of headroom.
pub fn prompt_config(
    endpoint: &SuggestionEndpoint,
    messages: &[SuggestionMessage],
) -> Option<StreamConfig> {
    if messages.last()?.role != "assistant" {
        return None;
    }
    if messages
        .iter()
        .any(|m| !matches!(m.role.as_str(), "user" | "assistant"))
    {
        return None;
    }
    let window = if endpoint.context_window == 0 {
        8192
    } else {
        endpoint.context_window as usize
    };
    let mut budget = HISTORY_TOKENS;
    let mut keep_from = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        if message.text.trim().is_empty() {
            return None;
        }
        let tokens = evot_engine::context::estimate_tokens(&serde_json::to_string(message).ok()?);
        if tokens > budget {
            break;
        }
        keep_from = index;
        budget = budget.saturating_sub(tokens);
    }
    let recent = &messages[keep_from..];
    if recent
        .iter()
        .filter(|message| message.role == "assistant")
        .count()
        < 2
        || !recent.iter().any(|message| message.role == "user")
    {
        return None;
    }
    // Same shape as Codex: a recent native-message suffix followed by one
    // prediction instruction. Never reintroduce the start of a long session.
    let final_instruction =
        format!("<next_prompt_suggestion>\n{INSTRUCTIONS}\n</next_prompt_suggestion>");
    let input_tokens = recent
        .iter()
        .map(|message| evot_engine::context::estimate_tokens(&message.text) + 16)
        .sum::<usize>()
        + evot_engine::context::estimate_tokens(&final_instruction)
        + 64;
    if input_tokens >= window.saturating_sub(TOKEN_HEADROOM) {
        return None;
    }
    let mut native_messages: Vec<Message> = recent
        .iter()
        .map(|message| {
            if message.role == "user" {
                Message::user(message.text.clone())
            } else {
                Message::Assistant {
                    content: vec![Content::Text {
                        text: message.text.clone(),
                    }],
                    stop_reason: StopReason::Stop,
                    model: String::new(),
                    provider: String::new(),
                    usage: Default::default(),
                    timestamp: 0,
                    error_message: None,
                    response_id: None,
                }
            }
        })
        .collect();
    native_messages.push(Message::user(final_instruction));
    let protocol = endpoint.parsed_protocol()?;
    let model_config = crate::conf::resolve_model_config(
        protocol,
        "evot-suggestion",
        &endpoint.model,
        Some(&endpoint.base_url),
        Default::default(),
        Default::default(),
        (endpoint.context_window > 0).then_some(endpoint.context_window),
        Some(OUTPUT_TOKENS),
        Some(false),
    );
    Some(StreamConfig {
        model: endpoint.model.clone(),
        api_key: endpoint.api_key.clone(),
        system_prompt: String::new(),
        messages: native_messages,
        tools: Vec::new(),
        thinking_level: ThinkingLevel::Off,
        max_tokens: Some(OUTPUT_TOKENS),
        model_config: Some(model_config),
        cache_config: Default::default(),
        prompt_cache_key: None,
        client: None,
    })
}

pub async fn sample(
    endpoint: &SuggestionEndpoint,
    messages: &[SuggestionMessage],
    provider: Arc<dyn StreamProvider>,
    cancel: CancellationToken,
) -> Option<String> {
    let config = prompt_config(endpoint, messages)?;
    let child = cancel.child_token();
    let _guard = child.clone().drop_guard();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let call = provider.stream(config, tx, child);
    tokio::pin!(call);
    let started = std::time::Instant::now();
    let deadline = tokio::time::sleep(SUGGESTION_TIMEOUT);
    tokio::pin!(deadline);
    let outcome = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return None,
            _ = &mut deadline => {
                tracing::info!(target: "evot::suggestion", outcome = "timeout", elapsed_ms = started.elapsed().as_millis());
                return None;
            },
            result = &mut call => match result {
                Ok(outcome) => break outcome,
                Err(_) => {
                    // Do not log error bodies: an upstream may echo secrets
                    // or conversation fragments. Only the status is needed.
                    tracing::info!(target: "evot::suggestion", outcome = "provider_error", elapsed_ms = started.elapsed().as_millis());
                    return None;
                }
            },
            Some(_) = rx.recv() => {},
        }
    };
    let Message::Assistant {
        content,
        stop_reason: StopReason::Stop,
        ..
    } = outcome.into_message()
    else {
        tracing::info!(target: "evot::suggestion", outcome = "incomplete", elapsed_ms = started.elapsed().as_millis());
        return None;
    };
    let mut text = String::new();
    for block in content {
        match block {
            Content::Text { text: part } => text.push_str(&part),
            Content::ToolCall { .. } => return None,
            _ => {}
        }
    }
    let Some(filtered) = filter(&text) else {
        let outcome = if text.trim().is_empty() || text.trim().eq_ignore_ascii_case("NONE") {
            "no_obvious_next_step"
        } else {
            "filtered"
        };
        tracing::info!(target: "evot::suggestion", outcome, elapsed_ms = started.elapsed().as_millis());
        return None;
    };
    if filtered.starts_with('/') {
        tracing::info!(target: "evot::suggestion", outcome = "suggested_action", elapsed_ms = started.elapsed().as_millis());
        return Some(filtered);
    }
    let reference = messages
        .iter()
        .rev()
        .find(|message| message.role == "user")?;
    let matches = language_matches(&reference.text, &filtered);
    let outcome = if matches {
        "suggested"
    } else {
        "wrong_language"
    };
    tracing::info!(target: "evot::suggestion", outcome, elapsed_ms = started.elapsed().as_millis());
    matches.then_some(filtered)
}

/// Catch obvious script switches even if the model disregards the language
/// instruction. Natural-language identity (e.g. English vs French) remains
/// the model's job; identifiers and inline/fenced code are not evidence.
fn language_matches(reference: &str, suggestion: &str) -> bool {
    let prose = reference
        .split('`')
        .step_by(2)
        .collect::<Vec<_>>()
        .join(" ");
    let script = |ch: char| match ch {
        '\u{3040}'..='\u{30ff}' => 1, // kana
        '\u{ac00}'..='\u{d7af}' => 2, // hangul
        '\u{4e00}'..='\u{9fff}' => 3, // han
        '\u{0400}'..='\u{052f}' => 4, // cyrillic
        '\u{0600}'..='\u{06ff}' => 5, // arabic
        '\u{0900}'..='\u{097f}' => 6, // devanagari
        _ => 0,
    };
    // Kana takes priority over shared Han; do not mistake Japanese for Chinese.
    let expected = if prose.chars().any(|ch| script(ch) == 1) {
        1
    } else if prose.chars().any(|ch| script(ch) == 2) {
        2
    } else {
        prose
            .chars()
            .map(script)
            .find(|value| *value != 0)
            .unwrap_or(0)
    };
    if expected == 0 {
        !suggestion.chars().any(|ch| script(ch) != 0)
    } else {
        suggestion.chars().any(|ch| script(ch) == expected)
    }
}

fn filter(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty()
        || text.eq_ignore_ascii_case("NONE")
        || text.chars().count() > 160
        || text.starts_with(['`', '"', '“', '<'])
        || text
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '?' | '？'))
    {
        return None;
    }
    Some(text.to_string())
}
