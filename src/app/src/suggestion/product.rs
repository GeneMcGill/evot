use serde::Deserialize;
use serde::Serialize;

/// Optional, bounded UI facts. Kept separate from conversation roles and the
/// Codex prompt; older addon callers can omit this argument entirely.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SuggestionProductContext {
    pub trigger: String,
    pub mode: String,
    pub state: String,
    pub input_behavior: String,
    pub actions: Vec<SuggestionAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_task: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct SuggestionAction {
    pub text: String,
    pub description: String,
}

impl SuggestionProductContext {
    pub(super) fn valid(&self) -> bool {
        matches!(
            self.trigger.as_str(),
            "answer" | "resume" | "mode_changed" | "loop_waiting"
        ) && matches!(self.mode.as_str(), "normal" | "plan" | "log")
            && matches!(
                self.state.as_str(),
                "idle" | "waiting-background" | "waiting-loop"
            )
            && self.input_behavior.chars().count() <= 512
            && self
                .original_task
                .as_ref()
                .is_none_or(|task| task.chars().count() <= 512)
            && self.actions.len() <= 8
            && self.actions.iter().all(|action| {
                action.text.starts_with('/')
                    && action.text.chars().count() <= 80
                    && !action.text.chars().any(char::is_control)
                    && action.description.chars().count() <= 160
            })
    }

    pub(super) fn allows_command(&self, text: &str) -> bool {
        self.valid() && self.actions.iter().any(|action| action.text == text)
    }
}
