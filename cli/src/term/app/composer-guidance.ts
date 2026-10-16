import { resolveCommand } from '../../commands/index.js'
import type { RunInteractionState } from './run-interaction.js'
import type { SuggestionAction, SuggestionProductContext, SuggestionTrigger } from '../../native/contracts/suggestion.js'
export type { SuggestionAction, SuggestionProductContext, SuggestionTrigger } from '../../native/contracts/suggestion.js'

export type ComposerMode = SuggestionProductContext['mode']
export const MODE_HINTS = {
  normal: { hint: 'Enter a coding task or / for commands', shortHint: 'Enter a coding task' },
  plan: { hint: 'Describe what to plan · no edits · /done to return', shortHint: 'Describe what to plan · /done' },
  log: { hint: 'Ask about these logs · /done to return', shortHint: 'Ask about logs · /done' },
} as const

export interface ComposerGuidance {
  mode: ComposerMode
  state: RunInteractionState['kind']
  placeholder: string
  shortPlaceholder: string
  /** Remains visible when model ghost text replaces the placeholder. */
  operationHint?: string
  allowSuggestion: boolean
  inputBehavior: string
  actions: SuggestionAction[]
}

/** One policy for rendering, sampling and Tab acceptance. State outranks mode;
 * menus/drafts/attachments are host-owned eligibility, not a second run state. */
export function resolveComposerGuidance(input: {
  interaction: RunInteractionState
  mode: ComposerMode
  queuedCount?: number
}): ComposerGuidance {
  const { interaction, mode } = input
  const state = interaction.kind
  const base = MODE_HINTS[mode]
  const guidance: ComposerGuidance = {
    mode, state, placeholder: base.hint, shortPlaceholder: base.shortHint,
    allowSuggestion: mode !== 'log' && ['idle', 'waiting-loop', 'waiting-background'].includes(state),
    inputBehavior: mode === 'plan' ? 'Plan the next task without editing files.'
      : mode === 'log' ? 'Analyze logs in a separate read-only conversation.' : 'Send a new message to the current conversation.',
    actions: mode === 'normal' ? [] : [{ text: '/done', description: mode === 'log' ? 'Return to the previous conversation mode.' : 'Return to normal conversation mode.' }],
    operationHint: mode === 'plan' ? 'Plan only · no edits · /done to return' : undefined,
  }
  if (state === 'waiting-loop') {
    guidance.placeholder = 'Loop waiting · type to act now · /loop 15m to change pace · esc twice to stop'
    guidance.shortPlaceholder = 'Loop waiting · type · /loop 15m · esc twice'
    guidance.operationHint = 'esc twice to stop · type to act now · /loop 15m to change pace'
    if (mode === 'plan') guidance.operationHint = 'esc twice to stop · no edits · /done · /loop 15m'
    guidance.inputBehavior = `${guidance.inputBehavior} The loop is waiting; a message acts now instead of waiting for its next scheduled turn.`
    guidance.actions.push(
      { text: '/loop 15m', description: 'Change the loop interval to fifteen minutes.' },
      { text: '/loop stop', description: 'Stop this loop.' },
    )
  } else if (state === 'compacting') {
    guidance.placeholder = 'Compacting context · input queues for after compaction'
    guidance.shortPlaceholder = 'Compacting · input queues'
    guidance.inputBehavior = 'Queue the message until context compaction finishes.'
  } else if (state === 'local-operation') {
    guidance.placeholder = 'Local operation in progress · please wait'
    guidance.shortPlaceholder = 'Local operation · please wait'
    guidance.inputBehavior = 'Wait for the local operation to finish.'
  } else if (state === 'retrying') {
    guidance.placeholder = 'Waiting to retry · input queues for recovery · esc twice to stop'
    guidance.shortPlaceholder = 'Retrying · input queues · esc twice'
    guidance.inputBehavior = 'Queue input until the current request recovers.'
  } else if (state === 'active' || state === 'waiting-task') {
    guidance.placeholder = 'Guide the agent while it works'
    guidance.shortPlaceholder = 'Guide while it works'
    guidance.inputBehavior = 'Steer the running agent at its next message boundary.'
  }
  const queued = input.queuedCount ?? 0
  if (queued > 0) {
    guidance.allowSuggestion = false
    if (state === 'compacting') {
      guidance.placeholder = `${queued} queued · will run after compaction`
      guidance.shortPlaceholder = `${queued} queued · after compaction`
    } else if (state === 'active' || state === 'waiting-task' || state === 'retrying') {
      guidance.placeholder = `${queued} ${queued === 1 ? 'message' : 'messages'} queued · Ctrl+G to manage`
      guidance.shortPlaceholder = `${queued} queued · Ctrl+G`
    }
  }
  return guidance
}

export function suggestionProductContext(guidance: ComposerGuidance, trigger: SuggestionTrigger): SuggestionProductContext {
  const state = guidance.state
  if (!guidance.allowSuggestion || (state !== 'idle' && state !== 'waiting-background' && state !== 'waiting-loop')) {
    throw new Error('Suggestions are not available in this interaction state')
  }
  return { trigger, mode: guidance.mode, state,
    input_behavior: guidance.inputBehavior, actions: guidance.actions }
}

/** Only exact registered actions offered by this state may be suggested.
 * No free-form model-generated flags, arguments, paths or command chaining. */
export function allowedSuggestionCommand(text: string, actions: readonly SuggestionAction[]): boolean {
  return actions.some(action => action.text === text) && resolveCommand(text).kind === 'resolved'
}
