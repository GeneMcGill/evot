//! Curated metadata for models Evot actively supports.
//!
//! Provider specifications are authoritative for capacity. External agent
//! registries only inform agent-specific policy such as compaction thresholds.

mod anthropic;
mod deepseek;
mod glm;
mod google;
mod kimi;
mod openai;
mod profile;
mod xai;

use super::capabilities::InputModality;
use super::capabilities::ModelCapabilities;
use super::capabilities::ReasoningCapabilities;

pub(super) fn resolve(id: &str) -> Option<ModelCapabilities> {
    if id.is_empty() {
        return None;
    }

    openai::resolve(id)
        .or_else(|| anthropic::resolve(id))
        .or_else(|| deepseek::resolve(id))
        .or_else(|| glm::resolve(id))
        .or_else(|| google::resolve(id))
        .or_else(|| kimi::resolve(id))
        .or_else(|| xai::resolve(id))
        .or_else(|| openai::fallback(id))
        .or_else(|| anthropic::fallback(id))
        .or_else(|| glm::fallback(id))
        .or_else(|| google::fallback(id))
        .or_else(|| deepseek::fallback(id))
        .or_else(|| kimi::fallback(id))
        .or_else(|| xai::fallback(id))
        .map(profile::ModelProfile::capabilities)
}

pub(super) fn normalize_model_id(model_id: &str) -> String {
    let normalized = model_id.trim().to_ascii_lowercase();
    for prefix in [
        "openai/",
        "xai/",
        "x-ai/",
        "anthropic/",
        "deepseek/",
        "google/",
        "zai/",
        "z-ai/",
        "moonshotai/",
        "moonshotai-cn/",
    ] {
        if let Some(rest) = normalized.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    normalized
}

pub(super) fn protocol_fallback(vision: bool) -> ModelCapabilities {
    // Unknown/new models get the modern 1M-class default window; explicit
    // catalog entries and user config overrides still win.
    let max_input_tokens = 1_000_000;
    ModelCapabilities {
        max_input_tokens,
        advertised_context_window: max_input_tokens,
        max_output_tokens: if vision { 8_192 } else { 32_768 },
        input: if vision {
            vec![InputModality::Text, InputModality::Image]
        } else {
            vec![InputModality::Text]
        },
        reasoning: ReasoningCapabilities::new(
            profile::levels_map(profile::NO_REASONING.levels),
            profile::NO_REASONING.default,
            None,
        ),
        default_verbosity: None,
        compaction_limit: None,
        remote_compaction: false,
    }
}
