/** Optional second argument to suggestNextPrompt; legacy callers still send
 * only the original messages array. These are bounded UI facts, not prompts. */
export type SuggestionTrigger = 'answer' | 'resume' | 'mode_changed' | 'loop_waiting'
export interface SuggestionAction { text: string; description: string }
export interface SuggestionProductContext {
  trigger: SuggestionTrigger
  mode: 'normal' | 'plan' | 'log'
  state: 'idle' | 'waiting-background' | 'waiting-loop'
  input_behavior: string
  actions: SuggestionAction[]
  /** Bounded first task, captured before resume display/history truncation. */
  original_task?: string
}
