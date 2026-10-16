import type { ConfigInfo } from '../../native/contracts/config-info.js'
import type { AppState } from '../app/state.js'
import { providerDisplayName } from '../app/provider.js'
import { shouldDownOpenPanel } from '../app/background-panel.js'
import { isEditorEmpty, type EditorState } from '../input/editor.js'
import type { PromptVMInput } from './prompt.js'
import { resolveComposerGuidance } from '../app/composer-guidance.js'
import { resolveRunInteraction, type RunInteractionState } from '../app/run-interaction.js'

export interface PromptSnapshot {
  editor: EditorState
  session: Pick<AppState, 'model' | 'cwd' | 'sessionTokens'> & Partial<Pick<AppState, 'forkTrail' | 'cloudBadge'>>
  config?: ConfigInfo
  active: boolean
  planning: boolean
  logMode: boolean
  dashboardUrl: string | null
  exitHint: boolean
  columns: number
  rows: number
  gitBranch: string | null
  backgroundProcessCount: number
  backgroundStopHint?: string
  backgroundStopPending?: boolean
  /** A turn is streaming: the empty composer says typing steers it. */
  busy?: boolean
  /** Model suggestion is separate from slash-command ghost hints. */
  suggestion?: string
  interaction?: RunInteractionState
  /** A `/loop` is parked between ticks. */
  loopParked?: boolean
  /** Prompts queued behind the running turn. */
  queuedCount?: number
}

/** Pure projection of a single host snapshot. No agent, terminal or clocks. */
export function promptFromSnapshot(input: PromptSnapshot): PromptVMInput {
  const { editor, session, config } = input
  const empty = isEditorEmpty(editor)
  const active = config?.availableModels.find(model => model.model === session.model && model.provider === (config?.provider ?? ''))
    ?? config?.availableModels.find(model => model.model === session.model)
  return {
    lines: editor.lines,
    cursorLine: editor.cursorLine,
    cursorCol: editor.cursorCol,
    active: input.active,
    model: session.model,
    provider: providerDisplayName(active, config?.provider ?? ''),
    planning: input.planning,
    logMode: input.logMode,
    dashboardUrl: input.dashboardUrl,
    exitHint: input.exitHint,
    completion: editor.completion,
    ghostHint: editor.ghostHint,
    columns: input.columns,
    rows: input.rows,
    placeholder: empty,
    cwd: session.cwd,
    forkTrail: session.forkTrail ?? [],
    cloudBadge: session.cloudBadge ?? '',
    gitBranch: input.gitBranch,
    contextTokens: session.sessionTokens.contextTokens,
    contextWindow: session.sessionTokens.contextWindow,
    backgroundProcessCount: input.backgroundProcessCount,
    backgroundStopHint: input.backgroundStopHint,
    backgroundStopPending: input.backgroundStopPending,
    backgroundPanelDownAvailable: shouldDownOpenPanel({ editorEmpty: empty, running: input.backgroundProcessCount }),
    busy: input.busy ?? false,
    guidance: resolveComposerGuidance({
      interaction: input.interaction ?? resolveRunInteraction({ active: input.busy ?? false, owner: null, loopParked: input.loopParked }),
      mode: input.logMode ? 'log' : input.planning ? 'plan' : 'normal',
      queuedCount: input.queuedCount,
    }),
    ...(input.suggestion ? { suggestion: input.suggestion } : {}),
    loopParked: input.loopParked ?? false,
    queuedCount: input.queuedCount ?? 0,
    thinkingLevel: config?.thinkingLevel ?? '',
    ...(active?.thinking_levels?.length ? { thinkingLevels: active.thinking_levels } : {}),
  }
}
