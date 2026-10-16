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
use super::SuggestionProductContext;

// Match Codex's safety deadline, including cold connection/gateway latency.
// This is not a display delay: completion is displayed immediately and input
// cancels instantly. Foreground interaction never waits for this request.
pub const SUGGESTION_TIMEOUT: Duration = Duration::from_secs(8);
const HISTORY_TOKENS: usize = 1_536;
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

LANGUAGE IS MANDATORY: use the natural language of language_reference (the most recent user message), even if the assistant or earlier messages use another language. Infer language from the user's prose, not code, paths, identifiers or product names. Never translate the suggestion into the assistant's language."#;

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
    generate_with_context(messages, None, cancel).await
}

pub async fn generate_with_context(
    messages: Vec<SuggestionMessage>,
    product: Option<SuggestionProductContext>,
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
        sample_with_context(
            &endpoint,
            &messages,
            product.as_ref(),
            provider,
            cancel.clone(),
        )
        .await
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

/// Shared by transports and tests. Input is bounded independently of callers;
/// a small endpoint window reduces history further, rather than enlarging it.
pub fn prompt_config(
    endpoint: &SuggestionEndpoint,
    messages: &[SuggestionMessage],
) -> Option<StreamConfig> {
    prompt_config_with_context(endpoint, messages, None)
}

pub fn prompt_config_with_context(
    endpoint: &SuggestionEndpoint,
    messages: &[SuggestionMessage],
    product: Option<&SuggestionProductContext>,
) -> Option<StreamConfig> {
    if product.is_some_and(|context| !context.valid()) {
        return None;
    }
    let require_answer = product.is_none_or(|context| context.trigger == "answer");
    if (require_answer && messages.last()?.role != "assistant")
        || !messages.iter().any(|m| m.role == "user")
    {
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
    let mut budget = window
        .saturating_sub(512 + OUTPUT_TOKENS as usize)
        .min(HISTORY_TOKENS);
    let mut recent = Vec::new();
    for message in messages.iter().rev().take(12) {
        if budget < 32 {
            break;
        }
        // One character is a conservative token budget for mixed CJK/ASCII.
        // Keep both ends so a long answer retains its conclusion as well as
        // its opening. JSON/role overhead is reserved separately.
        let cap = budget.saturating_sub(24).min(512);
        let text = bounded_text(&message.text, cap);
        if text.trim().is_empty() {
            continue;
        }
        budget = budget.saturating_sub(text.chars().count() + 24);
        recent.push(SuggestionMessage {
            role: message.role.clone(),
            text,
        });
    }
    recent.reverse();
    if (require_answer && recent.last()?.role != "assistant")
        || !recent.iter().any(|m| m.role == "user")
    {
        return None;
    }
    let language_reference = &recent
        .iter()
        .rev()
        .find(|message| message.role == "user")?
        .text;
    let original_task = product
        .and_then(|context| context.original_task.as_deref())
        .filter(|task| !task.trim().is_empty())
        .or_else(|| {
            messages
                .iter()
                .find(|message| message.role == "user")
                .map(|message| message.text.as_str())
        })?;
    let mut context = serde_json::json!({
        "language_reference": language_reference,
        "original_task": bounded_text(original_task, 512),
    });
    if let Some(product) = product {
        let mut value = serde_json::to_value(product).ok()?;
        if let Some(fields) = value.as_object_mut() {
            fields.remove("original_task");
        }
        context["product_context"] = value;
    }
    // Retain causal roles instead of flattening the conversation into JSON.
    // Like Codex, append the prediction instruction as the final user turn.
    // Bounded UI facts and the independently retained initial task are data,
    // not a replacement system prompt or a fabricated assistant response.
    let context_json = serde_json::to_string(&context)
        .ok()?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e");
    let final_instruction = format!(
        "<suggestion_context>\n{context_json}\n</suggestion_context>\n\n<next_prompt_suggestion>\n{INSTRUCTIONS}\n</next_prompt_suggestion>",
    );
    let input_tokens = recent
        .iter()
        .map(|message| evot_engine::context::estimate_tokens(&message.text) + 16)
        .sum::<usize>()
        + evot_engine::context::estimate_tokens(&final_instruction)
        + 64;
    if input_tokens + OUTPUT_TOKENS as usize > window {
        return None;
    }
    let mut native_messages: Vec<Message> = recent
        .into_iter()
        .map(|message| {
            if message.role == "user" {
                Message::user(message.text)
            } else {
                Message::Assistant {
                    content: vec![Content::Text { text: message.text }],
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
    })
}

fn bounded_text(text: &str, cap: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= cap {
        return text.to_string();
    }
    let head = cap / 3;
    let tail = cap.saturating_sub(head + 1);
    chars[..head]
        .iter()
        .chain(std::iter::once(&'…'))
        .chain(chars[chars.len() - tail..].iter())
        .collect()
}

pub async fn sample(
    endpoint: &SuggestionEndpoint,
    messages: &[SuggestionMessage],
    provider: Arc<dyn StreamProvider>,
    cancel: CancellationToken,
) -> Option<String> {
    sample_with_context(endpoint, messages, None, provider, cancel).await
}

pub async fn sample_with_context(
    endpoint: &SuggestionEndpoint,
    messages: &[SuggestionMessage],
    product: Option<&SuggestionProductContext>,
    provider: Arc<dyn StreamProvider>,
    cancel: CancellationToken,
) -> Option<String> {
    let config = prompt_config_with_context(endpoint, messages, product)?;
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
    let Some(filtered) = filter(&text, product) else {
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

fn filter(text: &str, product: Option<&SuggestionProductContext>) -> Option<String> {
    let text = text.trim();
    if text.is_empty()
        || text.eq_ignore_ascii_case("NONE")
        || text.chars().count() > 160
        || text.starts_with(['`', '"', '“', '<'])
        || (text.starts_with('/') && !product.is_some_and(|context| context.allows_command(text)))
        || text
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '?' | '？'))
    {
        return None;
    }
    Some(text.to_string())
}
