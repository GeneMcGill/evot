import { expect, test } from 'bun:test'
import { resolveComposerGuidance, suggestionProductContext, allowedSuggestionCommand } from '../src/term/app/composer-guidance.js'
import { resolveRunInteraction, type RunInteractionState } from '../src/term/app/run-interaction.js'
import { buildPromptBlocks } from '../src/term/viewmodel/prompt.js'
import { promptFromSnapshot } from '../src/term/viewmodel/prompt-snapshot.js'
import { createEditorState } from '../src/term/input/editor.js'
import { createInitialState } from '../src/term/app/state.js'

const interaction = (kind: RunInteractionState['kind']) => ({ ...resolveRunInteraction({ active: false, owner: null }), kind })
const policy = (kind: RunInteractionState['kind'], mode: 'normal' | 'plan' | 'log' = 'normal', queuedCount = 0) =>
  resolveComposerGuidance({ interaction: interaction(kind), mode, queuedCount })

function rendered(kind: RunInteractionState['kind'], planning = false, suggestion = '') {
  const vm = promptFromSnapshot({ editor: createEditorState(), session: createInitialState('test', '/tmp'),
    active: true, planning, logMode: false, dashboardUrl: null, exitHint: false,
    columns: 180, rows: 32, gitBranch: null, backgroundProcessCount: 0,
    interaction: interaction(kind), suggestion })
  return buildPromptBlocks(vm).flatMap(block => block.lines).map(line => line.spans.map(span => span.text).join('')).join('\n')
}

test('sampling and display share one state and mode matrix', () => {
  for (const state of ['idle', 'waiting-loop', 'waiting-background'] as const) {
    expect(policy(state).allowSuggestion).toBe(true)
    expect(policy(state, 'plan').allowSuggestion).toBe(true)
    expect(policy(state, 'log').allowSuggestion).toBe(false)
  }
  for (const state of ['active', 'retrying', 'compacting', 'local-operation', 'waiting-task'] as const) {
    for (const mode of ['normal', 'plan', 'log'] as const) expect(policy(state, mode).allowSuggestion).toBe(false)
  }
  expect(policy('idle', 'normal', 1).allowSuggestion).toBe(false)
})

test('waiting, compaction, retries and local operations describe actual input behavior', () => {
  expect(rendered('active')).toContain('Guide the agent while it works')
  expect(rendered('compacting')).toContain('input queues for after compaction')
  expect(rendered('retrying')).toContain('input queues for recovery')
  expect(rendered('local-operation')).toContain('Local operation in progress')
  expect(rendered('waiting-loop')).toContain('type to act now')
  expect(policy('compacting', 'normal', 2).placeholder).not.toContain('Ctrl+G')
  expect(policy('active', 'normal', 2).placeholder).toContain('Ctrl+G')
})

test('ghost text never removes loop controls or the plan no-edit warning', () => {
  const loop = rendered('waiting-loop', false, 'Run the targeted tests')
  expect(loop).toContain('Run the targeted tests')
  expect(loop).toContain('esc twice to stop')
  expect(loop).toContain('/loop 15m')
  const plan = rendered('idle', true, 'Review the design')
  expect(plan).toContain('Review the design')
  expect(plan).toContain('no edits')
  expect(plan).toContain('/done')
  expect(rendered('retrying', false, 'stale suggestion')).not.toContain('stale suggestion')
  expect(rendered('compacting', false, 'stale suggestion')).not.toContain('stale suggestion')
})

test('only exact registered actions exposed by the current state are acceptable', () => {
  const actions = policy('waiting-loop', 'plan').actions
  for (const command of ['/done', '/loop 15m', '/loop stop']) expect(allowedSuggestionCommand(command, actions)).toBe(true)
  for (const command of ['/new', '/loop stop; rm -rf .', '/loop 15m extra', '/done\n/new']) {
    expect(allowedSuggestionCommand(command, actions)).toBe(false)
  }
  expect(allowedSuggestionCommand('/not_registered', [{ text: '/not_registered', description: 'Invalid' }])).toBe(false)
  expect(allowedSuggestionCommand('/done', policy('idle').actions)).toBe(false)
})

test('product facts are short and state-specific, not a rewrite of the Codex prompt', () => {
  expect(suggestionProductContext(policy('waiting-loop'), 'loop_waiting')).toEqual({
    trigger: 'loop_waiting', mode: 'normal', state: 'waiting-loop',
    input_behavior: policy('waiting-loop').inputBehavior,
    actions: [{ text: '/loop 15m', description: 'Change the loop interval to fifteen minutes.' },
      { text: '/loop stop', description: 'Stop this loop.' }],
  })
  expect(suggestionProductContext(policy('idle', 'plan'), 'mode_changed').actions[0]?.text).toBe('/done')
  expect(() => suggestionProductContext(policy('retrying'), 'answer')).toThrow()
})
