//! Tests for compaction budget derivation — how a model's context window and
//! user settings become trigger/retention budgets.

use evotengine::context::CompactionConfig;
use evotengine::context::ContextConfig;
use evotengine::context::DEFAULT_KEEP_RECENT_TOKENS;
use evotengine::context::DEFAULT_POST_COMPACTION_TOKENS;
use evotengine::context::DEFAULT_RESERVE_TOKENS;
use evotengine::context::DEFAULT_SUMMARY_MAX_BYTES;
use evotengine::context::DEFAULT_SUMMARY_RESERVE_TOKENS;
use evotengine::context::SUMMARIZER_INPUT_MAX_BYTES;
use evotengine::provider::ModelConfig;
use evotengine::ThinkingLevel;

/// Trigger headroom is fixed rather than scaled with the model window. The
/// retained tail is independently bounded by the post-compaction envelope.
#[test]
fn budgets_are_fixed_regardless_of_window_size() {
    for window in [128_000, 272_000, 1_048_576] {
        let cfg = CompactionConfig::from_context_window(window);
        assert_eq!(cfg.reserve_tokens, DEFAULT_RESERVE_TOKENS, "{window}");
        assert_eq!(
            cfg.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS,
            "{window}"
        );
        assert_eq!(cfg.trigger_threshold(), window - DEFAULT_RESERVE_TOKENS);
        assert_eq!(cfg.summary_max_bytes, DEFAULT_SUMMARY_MAX_BYTES);
        assert_eq!(
            cfg.summarizer_input_max_bytes(),
            (window - DEFAULT_RESERVE_TOKENS)
                .saturating_mul(4)
                .min(SUMMARIZER_INPUT_MAX_BYTES)
        );
    }
}

#[test]
fn retained_tail_fits_the_post_compaction_envelope() {
    let cfg = CompactionConfig::default();
    assert_eq!(DEFAULT_POST_COMPACTION_TOKENS, 40_000);
    assert_eq!(DEFAULT_SUMMARY_RESERVE_TOKENS, 4_000);
    assert_eq!(DEFAULT_KEEP_RECENT_TOKENS, 36_000);
    assert_eq!(cfg.retained_tail_budget(0), 36_000);
    assert_eq!(cfg.retained_tail_budget(5_000), 31_000);
    assert_eq!(DEFAULT_SUMMARY_MAX_BYTES, 32 * 1024);

    let mut custom = cfg;
    custom.keep_recent_tokens = 1_000;
    assert_eq!(custom.retained_tail_budget(5_000), 1_000);

    let small = CompactionConfig::from_context_window(10_000);
    assert_eq!(small.retained_tail_budget(500), 5_500);

    // Summary reserve caps at half the post target so tiny windows still
    // leave room for a retained tail.
    let tiny = CompactionConfig::from_context_window(1_000);
    assert_eq!(tiny.summary_reserve_tokens(), 500);
    assert_eq!(tiny.summary_max_bytes(), 2_000);
    assert_eq!(tiny.retained_tail_budget(125), 375);
}

#[test]
fn context_config_defaults_leave_budgets_unset() {
    let cfg = CompactionConfig::from_context_config(&ContextConfig::from_context_window(272_000));
    assert_eq!(cfg.context_window, 272_000);
    assert_eq!(cfg.reserve_tokens, DEFAULT_RESERVE_TOKENS);
    assert_eq!(cfg.keep_recent_tokens, DEFAULT_KEEP_RECENT_TOKENS);
}

/// Explicit settings win, so a caller can shrink budgets to fit a small window.
#[test]
fn explicit_budgets_override_the_defaults() {
    let cfg = CompactionConfig::from_context_config(&ContextConfig {
        max_context_tokens: 1_000,
        system_prompt_tokens: 0,
        advertised_context_window: None,
        reserve_tokens: Some(125),
        trigger_tokens: None,
        keep_recent_tokens: Some(200),
    });
    assert_eq!(cfg.reserve_tokens, 125);
    assert_eq!(cfg.keep_recent_tokens, 200);
    assert_eq!(cfg.trigger_threshold(), 875);
    assert_eq!(cfg.summarizer_input_max_bytes(), 3_500);
}

/// A window smaller than the default reserve must not wrap around into a huge
/// threshold; saturating arithmetic floors it at zero (always compact).
#[test]
fn threshold_saturates_when_window_is_below_the_reserve() {
    let cfg = CompactionConfig::from_context_window(8_000);
    assert_eq!(cfg.trigger_threshold(), 0);
    assert_eq!(cfg.summarizer_input_max_bytes(), 16_000);
}

#[test]
fn context_config_uses_profile_compaction_policy() {
    // Large-window models have no profile limit: the trigger falls back to
    // `window - reserve` (pi-style), so the whole window is usable.
    let gpt = ModelConfig::openai("gpt-5.5", "GPT-5.5");
    let context = ContextConfig::from_model(&gpt, ThinkingLevel::Medium);
    assert_eq!(context.max_context_tokens, 922_000);
    assert_eq!(context.trigger_tokens, None);
    assert_eq!(
        CompactionConfig::from_context_config(&context).trigger_threshold(),
        922_000 - DEFAULT_RESERVE_TOKENS
    );

    let claude = ModelConfig::anthropic("claude-sonnet-4-20250514", "Claude Sonnet 4");
    let context = ContextConfig::from_model(&claude, ThinkingLevel::High);
    assert_eq!(context.max_context_tokens, 200_000);
    assert_eq!(context.trigger_tokens, Some(180_000));

    let unknown = ModelConfig::local("http://localhost:11434/v1", "some/model");
    let context = ContextConfig::from_model(&unknown, ThinkingLevel::Off);
    assert_eq!(context.max_context_tokens, 1_000_000);
    assert_eq!(context.trigger_tokens, None);
    assert_eq!(
        CompactionConfig::from_context_config(&context).trigger_threshold(),
        1_000_000 - DEFAULT_RESERVE_TOKENS
    );
}

#[test]
fn unknown_window_uses_the_transport_ceiling() {
    let cfg = CompactionConfig::from_context_window(0);
    assert_eq!(cfg.summarizer_input_max_bytes(), SUMMARIZER_INPUT_MAX_BYTES);
}
