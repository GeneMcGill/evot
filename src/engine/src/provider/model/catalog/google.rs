use super::profile::ModelProfile;
use super::profile::ReasoningProfile;
use super::profile::BASE;
use crate::ThinkingLevel;

// Gemini 4 Argon always thinks (models.dev `reasoning_options`: effort
// low / medium / high, no toggle). Medium is the shipped default.
const GEMINI_4_LEVELS: &[(ThinkingLevel, Option<&str>)] = &[
    (ThinkingLevel::Low, Some("low")),
    (ThinkingLevel::Medium, Some("medium")),
    (ThinkingLevel::High, Some("high")),
];
const GEMINI_4_REASONING: ReasoningProfile = ReasoningProfile {
    levels: GEMINI_4_LEVELS,
    default: ThinkingLevel::Medium,
    anthropic_wire: None,
};

// 1_048_576 total window; the Gemini API caps output at 32_768 (Vertex
// allows 65_536, but the smaller figure is safe on every route).
const GEMINI_4_ARGON: ModelProfile = ModelProfile {
    max_input_tokens: 1_015_808,
    advertised_context_window: Some(1_048_576),
    max_output_tokens: 32_768,
    vision: true,
    reasoning: GEMINI_4_REASONING,
    ..BASE
};

#[rustfmt::skip]
const PROFILES: &[(&str, ModelProfile)] = &[
    ("gemini-4-argon", GEMINI_4_ARGON),
];

pub(super) fn resolve(id: &str) -> Option<ModelProfile> {
    PROFILES
        .iter()
        .find_map(|(candidate, profile)| (*candidate == id).then_some(*profile))
}

/// Uncatalogued Gemini 4+ ids inherit the Argon window and effort ladder.
pub(super) fn fallback(id: &str) -> Option<ModelProfile> {
    super::profile::version_at_least(id, "gemini-", (4, 0)).then_some(GEMINI_4_ARGON)
}
