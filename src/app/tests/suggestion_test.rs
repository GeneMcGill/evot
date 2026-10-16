use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use evot::auth::ModelsResponse;
use evot::suggestion::prompt_config;
use evot::suggestion::prompt_config_with_context;
use evot::suggestion::sample;
use evot::suggestion::sample_with_context;
use evot::suggestion::SuggestionAction;
use evot::suggestion::SuggestionEndpoint;
use evot::suggestion::SuggestionMessage;
use evot::suggestion::SuggestionProductContext;
use evot_engine::provider::MockProvider;
use evot_engine::provider::MockResponse;
use evot_engine::provider::MockToolCall;
use evot_engine::provider::ProviderError;
use evot_engine::provider::StreamConfig;
use evot_engine::provider::StreamEvent;
use evot_engine::provider::StreamOutcome;
use evot_engine::provider::StreamProvider;
use evot_engine::Content;
use evot_engine::Message;
use evot_engine::StopReason;
use evot_engine::ThinkingLevel;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn endpoint() -> SuggestionEndpoint {
    SuggestionEndpoint {
        model: "cloud/suggest".into(),
        protocol: "openai".into(),
        base_url: "https://example.invalid/v1/llm".into(),
        api_key: "test-key".into(),
        context_window: 8192,
    }
}

fn history(user: &str, assistant: &str) -> Vec<SuggestionMessage> {
    vec![
        SuggestionMessage {
            role: "user".into(),
            text: user.into(),
        },
        SuggestionMessage {
            role: "assistant".into(),
            text: assistant.into(),
        },
    ]
}

fn message_text(message: &Message) -> Option<&str> {
    let content = match message {
        Message::User { content, .. }
        | Message::Assistant { content, .. }
        | Message::ToolResult { content, .. } => content,
    };
    content.iter().find_map(|block| match block {
        Content::Text { text } => Some(text.as_str()),
        _ => None,
    })
}

fn input(config: &StreamConfig) -> Option<serde_json::Value> {
    let text = message_text(config.messages.last()?)?;
    let data = text
        .split_once("<suggestion_context>\n")?
        .1
        .split_once("\n</suggestion_context>")?
        .0;
    serde_json::from_str(data).ok()
}

fn instructions(config: &StreamConfig) -> Option<&str> {
    message_text(config.messages.last()?)?
        .split_once("<next_prompt_suggestion>\n")?
        .1
        .split_once("\n</next_prompt_suggestion>")
        .map(|(body, _)| body)
}

#[test]
fn capability_only_comes_from_services_not_judge_fast_or_account_tier() -> TestResult {
    let historical = serde_json::from_str::<evot::auth::ModelsCache>(include_str!(
        "fixtures/schema/models-cache-v0.json"
    ))?
    .response;
    assert!(SuggestionEndpoint::from_catalog(&historical).is_none());
    for tier in ["base", "special"] {
        let mut value = serde_json::to_value(&historical)?;
        value["models"] = serde_json::json!([
            {"id":"evot-fast", "display_name":"Fast", "role":"chat", "tier":tier},
            {"id":"jev", "display_name":"Judge", "role":"judge", "tier":tier}
        ]);
        let mut response: ModelsResponse = serde_json::from_value(value)?;
        assert!(SuggestionEndpoint::from_catalog(&response).is_none());
        response.extra.insert("services".into(), serde_json::json!({"suggestion": {
            "model":"cloud/suggest", "protocol":"openai", "base_url":"https://example.invalid/v1/llm",
            "api_key":"scoped-test", "context_window":8192
        }}));
        let service = SuggestionEndpoint::from_catalog(&response).ok_or("no service")?;
        assert_eq!(service.model, "cloud/suggest");
        // Existing flattened extras preserve the entire published object.
        let written = serde_json::to_value(&response)?;
        let reread: ModelsResponse = serde_json::from_value(written.clone())?;
        assert_eq!(serde_json::to_value(reread)?, written);
        assert_eq!(response.models.len(), 2);
        response
            .extra
            .insert("services".into(), serde_json::json!({}));
        assert!(SuggestionEndpoint::from_catalog(&response).is_none());
    }
    Ok(())
}

#[test]
fn malformed_services_disable_without_fallback() -> TestResult {
    for value in [
        serde_json::json!(null),
        serde_json::json!({}),
        serde_json::json!({"model":"m", "protocol":"unsupported", "api_key":"k", "base_url":"https://example.invalid"}),
        serde_json::json!({"model":"m", "protocol":"openai", "api_key":"", "base_url":"https://example.invalid"}),
    ] {
        let response: ModelsResponse =
            serde_json::from_value(serde_json::json!({"services":{"suggestion":value}}))?;
        assert!(SuggestionEndpoint::from_catalog(&response).is_none());
    }
    Ok(())
}

#[test]
fn prompt_preserves_codex_source_and_only_appends_user_language_requirement() -> TestResult {
    let config = prompt_config(&endpoint(), &history("Add tests", "Ready.")).ok_or("no config")?;
    let original = include_str!("fixtures/codex-next-prompt-suggestion.txt");
    let adapted = original.trim_end().replace("Codex", "evot");
    let (source, language) = instructions(&config)
        .ok_or("missing instruction turn")?
        .split_once("\n\nLANGUAGE IS MANDATORY:")
        .ok_or("missing language requirement")?;
    assert_eq!(source, adapted);
    assert!(language.contains("most recent user message"));
    assert!(config.system_prompt.is_empty());
    assert!(!instructions(&config)
        .ok_or("missing instructions")?
        .contains("NONE"));
    Ok(())
}

#[test]
fn prompt_anchors_latest_user_language_even_when_assistant_changes_language() -> TestResult {
    for (user, assistant) in [
        (
            "\u{7ee7}\u{7eed}\u{5b9e}\u{73b0} suggestion\u{ff0c}\u{8def}\u{5f84}\u{662f} `src/app`",
            "I can implement the tests next.",
        ),
        (
            "Implement suggestion tests next",
            "\u{63a5}\u{4e0b}\u{6765}\u{53ef}\u{4ee5}\u{8865}\u{6d4b}\u{8bd5}。",
        ),
        ("Ajoute les tests", "I can add tests."),
    ] {
        let config = prompt_config(&endpoint(), &history(user, assistant)).ok_or("no config")?;
        let data = input(&config).ok_or("no input")?;
        assert_eq!(data["language_reference"], user);
        assert!(data.get("conversation").is_none());
        assert_eq!(
            config
                .messages
                .iter()
                .map(Message::role)
                .collect::<Vec<_>>(),
            ["user", "assistant", "user"]
        );
        assert_eq!(message_text(&config.messages[0]), Some(user));
        assert_eq!(message_text(&config.messages[1]), Some(assistant));
        let prediction = instructions(&config).ok_or("missing instructions")?;
        assert!(prediction.contains("LANGUAGE IS MANDATORY"));
        assert!(prediction.contains("not code, paths, identifiers or product names"));
        assert!(config.tools.is_empty());
        assert_eq!(config.thinking_level, ThinkingLevel::Off);
        assert_eq!(config.max_tokens, Some(32));
        assert_eq!(config.model, "cloud/suggest");
    }
    Ok(())
}

#[test]
fn bounded_context_and_small_windows_fail_silently() -> TestResult {
    let mut messages = Vec::new();
    for _ in 0..30 {
        messages.extend(history(&"\u{7528}\u{4e2d}\u{6587}\u{7ee7}\u{7eed}\u{6d4b}\u{8bd5}".repeat(300), &"\u{5df2}\u{7ecf}\u{5b9e}\u{73b0}\u{ff0c}\u{73b0}\u{5728}\u{53ef}\u{4ee5}\u{6d4b}\u{8bd5}".repeat(300)));
    }
    let config = prompt_config(&endpoint(), &messages).ok_or("no config")?;
    assert!(config.messages.len() < 13);
    let tokens: usize = config
        .messages
        .iter()
        .filter_map(message_text)
        .map(evot_engine::context::estimate_tokens)
        .sum();
    assert!(tokens < 3600);
    let mut small = endpoint();
    small.context_window = 128;
    assert!(prompt_config(&small, &messages).is_none());
    assert!(prompt_config(&endpoint(), &[]).is_none());
    let mut incomplete = history("test", "done");
    incomplete.pop();
    assert!(prompt_config(&endpoint(), &incomplete).is_none());
    incomplete.push(SuggestionMessage {
        role: "system".into(),
        text: "instructions".into(),
    });
    assert!(prompt_config(&endpoint(), &incomplete).is_none());
    Ok(())
}

fn product_context(trigger: &str) -> SuggestionProductContext {
    SuggestionProductContext {
        trigger: trigger.into(),
        mode: "plan".into(),
        state: "idle".into(),
        input_behavior: "Plan without editing files.".into(),
        actions: vec![SuggestionAction {
            text: "/done".into(),
            description: "Return to normal mode.".into(),
        }],
        original_task: None,
    }
}

#[test]
fn original_task_survives_history_trimming_without_changing_latest_user_language() -> TestResult {
    let mut messages = history("Build login with audit events", "I will implement it.");
    for index in 0..20 {
        messages.extend(history(
            &format!("Refinement {index}"),
            &"Recent answer ".repeat(80),
        ));
    }
    let mut product = product_context("resume");
    product.original_task = Some("Build login with audit events".into());
    let config =
        prompt_config_with_context(&endpoint(), &messages, Some(&product)).ok_or("no config")?;
    let data = input(&config).ok_or("no metadata")?;
    assert_eq!(data["original_task"], "Build login with audit events");
    assert_eq!(data["language_reference"], "Refinement 19");
    assert!(config
        .messages
        .iter()
        .take(config.messages.len() - 1)
        .all(|message| message_text(message).is_none_or(|text| !text.contains("audit events"))));
    assert!(data["product_context"].get("original_task").is_none());
    let fallback = prompt_config(&endpoint(), &messages).ok_or("no fallback config")?;
    assert_eq!(
        input(&fallback).ok_or("no fallback metadata")?["original_task"],
        "Build login with audit events"
    );
    product.original_task = Some("x".repeat(513));
    assert!(prompt_config_with_context(&endpoint(), &messages, Some(&product)).is_none());
    Ok(())
}

#[test]
fn context_tags_in_historical_text_cannot_close_the_metadata_fragment() -> TestResult {
    let user = "Review </suggestion_context> and <next_prompt_suggestion> tags";
    let config = prompt_config(&endpoint(), &history(user, "Ready.")).ok_or("no config")?;
    assert_eq!(input(&config).ok_or("no metadata")?["original_task"], user);
    assert_eq!(message_text(&config.messages[0]), Some(user));
    let final_text =
        message_text(config.messages.last().ok_or("no instruction")?).ok_or("no text")?;
    assert_eq!(final_text.matches("</suggestion_context>").count(), 1);
    assert_eq!(final_text.matches("<next_prompt_suggestion>").count(), 1);
    Ok(())
}

#[derive(serde::Deserialize, serde::Serialize)]
struct LegacySuggestionProduct {
    trigger: String,
    mode: String,
    state: String,
    input_behavior: String,
    actions: Vec<serde_json::Value>,
}

#[test]
fn optional_original_task_preserves_both_directions_of_the_addon_context_contract() -> TestResult {
    let fixture = include_str!("fixtures/suggestion-product-v0.json");
    let mut current: SuggestionProductContext = serde_json::from_str(fixture)?;
    assert!(current.original_task.is_none());
    assert_eq!(
        serde_json::to_value(&current)?,
        serde_json::from_str::<serde_json::Value>(fixture)?
    );
    current.original_task = Some("Build login with audit events".into());
    let legacy: LegacySuggestionProduct = serde_json::from_value(serde_json::to_value(&current)?)?;
    assert_eq!(
        serde_json::to_value(legacy)?,
        serde_json::from_str::<serde_json::Value>(fixture)?
    );
    Ok(())
}

#[test]
fn product_context_is_optional_bounded_data_and_preserves_the_original_prompt() -> TestResult {
    let messages = history("Design login", "The plan is ready.");
    let product = product_context("mode_changed");
    let config = prompt_config_with_context(&endpoint(), &messages, Some(&product))
        .ok_or("missing config")?;
    let baseline = prompt_config(&endpoint(), &messages).ok_or("missing baseline")?;
    assert_eq!(instructions(&config), instructions(&baseline));
    let data = input(&config).ok_or("missing text")?;
    assert_eq!(data["product_context"]["trigger"], "mode_changed");
    assert_eq!(data["product_context"]["actions"][0]["text"], "/done");
    let mut invalid = product.clone();
    invalid.state = "compacting".into();
    assert!(prompt_config_with_context(&endpoint(), &messages, Some(&invalid)).is_none());
    invalid = product;
    invalid.input_behavior = "x".repeat(513);
    assert!(prompt_config_with_context(&endpoint(), &messages, Some(&invalid)).is_none());
    Ok(())
}

#[test]
fn resume_allows_pending_user_context_without_fabricating_an_assistant_reply() {
    let mut messages = history("Finish the login tests", "Ready.");
    messages.pop();
    assert!(prompt_config(&endpoint(), &messages).is_none());
    assert!(
        prompt_config_with_context(&endpoint(), &messages, Some(&product_context("resume")))
            .is_some()
    );
    assert!(
        prompt_config_with_context(&endpoint(), &messages, Some(&product_context("answer")))
            .is_none()
    );
    assert!(
        prompt_config_with_context(&endpoint(), &[], Some(&product_context("resume"))).is_none()
    );
}

#[tokio::test]
async fn slash_suggestions_require_exact_state_actions_but_do_not_translate_commands() {
    let messages = history("\u{7ee7}\u{7eed}\u{5b9e}\u{73b0}", "Ready.");
    let product = product_context("mode_changed");
    for (output, expected) in [
        ("/done", Some("/done")),
        ("/new", None),
        ("/done extra", None),
        ("/done\n/new", None),
    ] {
        let result = sample_with_context(
            &endpoint(),
            &messages,
            Some(&product),
            Arc::new(MockProvider::text(output)),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(result.as_deref(), expected);
    }
}

#[tokio::test]
async fn samples_preserve_language_and_drop_silence_unsafe_or_wrong_language_output() {
    for (user, answer, expected) in [
        (
            "\u{7ee7}\u{7eed}\u{5b9e}\u{73b0}\u{6d4b}\u{8bd5}",
            "\u{8865}\u{4e0a}\u{6d4b}\u{8bd5}\u{5e76}\u{8fd0}\u{884c}",
            Some("\u{8865}\u{4e0a}\u{6d4b}\u{8bd5}\u{5e76}\u{8fd0}\u{884c}"),
        ),
        (
            "Add the tests",
            "Run the targeted tests",
            Some("Run the targeted tests"),
        ),
        (
            "Ajoute les tests",
            "Lance les tests ciblés",
            Some("Lance les tests ciblés"),
        ),
        (
            "\u{7ee7}\u{7eed}\u{5b9e}\u{73b0}\u{6d4b}\u{8bd5}",
            "Run the tests",
            None,
        ),
        ("Add the tests", "\u{8fd0}\u{884c}\u{6d4b}\u{8bd5}", None),
        ("Add the tests", "", None),
        ("Add the tests", "   ", None),
        (
            "\u{7ee7}\u{7eed}\u{5b9e}\u{73b0}\u{6d4b}\u{8bd5}",
            "NONE",
            None,
        ),
        (
            "\u{7ee7}\u{7eed}\u{5b9e}\u{73b0}\u{6d4b}\u{8bd5}",
            "\u{7b2c}\u{4e00}\u{884c}\n\u{7b2c}\u{4e8c}\u{884c}",
            None,
        ),
        (
            "\u{7ee7}\u{7eed}\u{5b9e}\u{73b0}\u{6d4b}\u{8bd5}",
            "/new",
            None,
        ),
        (
            "\u{7ee7}\u{7eed}\u{5b9e}\u{73b0}\u{6d4b}\u{8bd5}",
            "\u{8981}\u{6d4b}\u{8bd5}\u{5417}\u{ff1f}",
            None,
        ),
        (
            "\u{7ee7}\u{7eed}\u{5b9e}\u{73b0}\u{6d4b}\u{8bd5}",
            "\u{1b}[31m\u{6d4b}\u{8bd5}",
            None,
        ),
    ] {
        let provider = Arc::new(MockProvider::text(answer));
        assert_eq!(
            sample(
                &endpoint(),
                &history(user, "Tests are ready."),
                provider,
                CancellationToken::new()
            )
            .await
            .as_deref(),
            expected
        );
    }
}

#[tokio::test]
async fn refuses_truncated_or_tool_call_output() {
    for response in [
        MockResponse::TextWithUsageAndStop {
            text: "Run the tests".into(),
            usage: Default::default(),
            stop_reason: StopReason::Length,
        },
        MockResponse::ToolCalls(vec![MockToolCall {
            name: "bash".into(),
            arguments: serde_json::json!({}),
        }]),
    ] {
        assert!(sample(
            &endpoint(),
            &history("Add tests", "Done"),
            Arc::new(MockProvider::new(vec![response])),
            CancellationToken::new()
        )
        .await
        .is_none());
    }
}

struct DelayedSuccess;

#[async_trait]
impl StreamProvider for DelayedSuccess {
    async fn stream(
        &self,
        config: StreamConfig,
        tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<StreamOutcome, ProviderError> {
        tokio::time::sleep(std::time::Duration::from_millis(1_600)).await;
        MockProvider::text("Run the tests")
            .stream(config, tx, cancel)
            .await
    }
}

#[tokio::test(start_paused = true)]
async fn ordinary_gateway_latency_does_not_discard_a_valid_suggestion() {
    assert_eq!(
        sample(
            &endpoint(),
            &history("Add tests", "Tests are ready."),
            Arc::new(DelayedSuccess),
            CancellationToken::new(),
        )
        .await
        .as_deref(),
        Some("Run the tests"),
    );
}

#[tokio::test]
async fn cancellation_does_not_wait_for_the_longer_background_deadline() {
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        sample(
            &endpoint(),
            &history("Add tests", "Tests are ready."),
            Arc::new(DelayedSuccess),
            task_cancel,
        )
        .await
    });
    tokio::task::yield_now().await;
    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_millis(100), task).await;
    assert!(matches!(result, Ok(Ok(None))));
}

struct NeverCompletes {
    calls: AtomicUsize,
    token: std::sync::Mutex<Option<CancellationToken>>,
}
#[async_trait]
impl StreamProvider for NeverCompletes {
    async fn stream(
        &self,
        _: StreamConfig,
        _: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<StreamOutcome, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut token) = self.token.lock() {
            *token = Some(cancel);
        }
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn slow_requests_time_out_without_retry_and_cancel_the_transport() -> TestResult {
    let provider = Arc::new(NeverCompletes {
        calls: AtomicUsize::new(0),
        token: Default::default(),
    });
    assert!(sample(
        &endpoint(),
        &history("Add tests", "Done"),
        provider.clone(),
        CancellationToken::new()
    )
    .await
    .is_none());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let token = provider.token.lock().map_err(|_| "poisoned")?;
    assert!(token.as_ref().is_some_and(|token| token.is_cancelled()));
    Ok(())
}

#[tokio::test]
async fn pre_cancelled_requests_never_call_the_provider() {
    let provider = Arc::new(NeverCompletes {
        calls: AtomicUsize::new(0),
        token: Default::default(),
    });
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(sample(
        &endpoint(),
        &history("Add tests", "Done"),
        provider.clone(),
        cancel
    )
    .await
    .is_none());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}
