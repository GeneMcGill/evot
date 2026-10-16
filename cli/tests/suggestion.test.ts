import { expect, test } from 'bun:test'
import { PromptSuggestion, suggestionContext, isSuggestionBoundary, suggestionHistoryFromLines } from '../src/term/app/suggestion.js'
import type { UIMessage } from '../src/term/app/types.js'
import { resolveComposerGuidance, allowedSuggestionCommand } from '../src/term/app/composer-guidance.js'
import { resolveRunInteraction } from '../src/term/app/run-interaction.js'

function messages(): UIMessage[] {
  return [
    { id: 'u0', role: 'user', text: 'Implement the tests', timestamp: 0 },
    { id: 'a0', role: 'assistant', text: 'The implementation is ready.', timestamp: 0 },
    { id: 'u', role: 'user', text: '\u7ee7\u7eed\u5b9e\u73b0 suggestion \u6d4b\u8bd5', timestamp: 1 },
    { id: 'a', role: 'assistant', text: '', timestamp: 2, content: [
      { type: 'thinking', text: 'private reasoning', contentIndex: 0 },
      { type: 'text', text: 'Tests are ready.', contentIndex: 1 },
    ] },
  ]
}

function harness(options: { allowsCommand?: (text: string) => boolean } = {}) {
  let key = 'session:1'
  let eligible = true
  let changes = 0
  const requests: { resolve(value: string | null): void; reject(error: Error): void; aborted: boolean }[] = []
  const controller = new PromptSuggestion({
    key: () => key, eligible: () => eligible, changed: () => changes++,
    ...options,
    start: () => {
      let resolve!: (value: string | null) => void
      let reject!: (error: Error) => void
      const result = new Promise<string | null>((yes, no) => { resolve = yes; reject = no })
      const request = { resolve, reject, aborted: false }
      requests.push(request)
      return { result: () => result, abort: () => { request.aborted = true } }
    },
  })
  return { controller, requests, setKey: (value: string) => { key = value },
    setEligible: (value: boolean) => { eligible = value }, changes: () => changes }
}

const tick = () => new Promise<void>(resolve => queueMicrotask(resolve))

test('context stops at incomplete tool flow instead of filling from older requests', () => {
  const history = messages()
  history.unshift({ id: 's', role: 'system', text: 'secret instructions', timestamp: 0 })
  history.splice(history.length - 1, 0, { id: 'tool', role: 'assistant', text: 'tool chatter', timestamp: 1, content: [
    { type: 'tool_call', contentIndex: 0, toolCall: { id: 't', name: 'bash', args: { command: 'secret' }, status: 'done' } },
  ] })
  expect(suggestionContext(history)).toEqual([{ role: 'assistant', text: 'Tests are ready.' }])
})

test('live context includes committed users even though AppState only retained assistant events', () => {
  const history = suggestionHistoryFromLines([
    { id: 'u', kind: 'user', text: 'Add the tests', zoneStart: true, zoneEnd: true },
    { id: 'thinking', kind: 'thinking', text: 'private reasoning' },
    { id: 'tool', kind: 'tool_result', text: 'private output' },
    { id: 'a1', kind: 'assistant', text: 'rendered first line', rawMarkdown: 'Tests are ready.', zoneStart: true },
    { id: 'a2', kind: 'assistant', text: 'rendered second line', rawMarkdown: 'Tests are ready.', zoneEnd: true },
  ])
  expect(suggestionContext(history)).toEqual([
    { role: 'user', text: 'Add the tests' },
    { role: 'assistant', text: 'Tests are ready.' },
  ])
  const h = harness()
  h.controller.start(history)
  expect(h.requests).toHaveLength(0)
})

test('context is a bounded contiguous suffix of whole messages', () => {
  const history = Array.from({ length: 30 }, (_, index): UIMessage => ({
    id: String(index), role: index % 2 ? 'assistant' : 'user', timestamp: index,
    text: 'x'.repeat(3000) + '\u7ed3\u8bba',
  }))
  const context = suggestionContext(history)
  expect(context).toHaveLength(5)
  expect(context.every(message => message.text === history[0]!.text)).toBe(true)
  expect(suggestionContext([...messages(), { ...messages()[3]!, content: undefined, text: 'x'.repeat(20000) }])).toEqual([])
  const oversized = { ...messages()[0]!, text: 'x'.repeat(20000) }
  expect(suggestionContext([messages()[0]!, oversized, ...messages().slice(-2)])).toEqual([
    { role: 'user', text: messages()[2]!.text }, { role: 'assistant', text: 'Tests are ready.' },
  ])
})

test('recognizes text-only answer boundaries, not deltas or tool-bearing responses', () => {
  const payload = { stop_reason: 'stop', content: [{ type: 'text', text: 'Done' }] }
  expect(isSuggestionBoundary({ kind: 'assistant_completed', payload })).toBe(true)
  for (const kind of ['assistant_delta', 'run_finished', 'tool_finished']) {
    expect(isSuggestionBoundary({ kind, payload })).toBe(false)
  }
  for (const stop_reason of ['length', 'error', 'toolUse', 'aborted']) {
    expect(isSuggestionBoundary({ kind: 'assistant_completed', payload: { ...payload, stop_reason } })).toBe(false)
  }
  expect(isSuggestionBoundary({ kind: 'assistant_completed', payload: {
    ...payload, content: [...payload.content, { type: 'tool_call' }],
  } })).toBe(false)
})

test('result is not editor text; Tab take is explicit, single-use and does not submit', async () => {
  const { controller, requests } = harness()
  controller.start(messages())
  expect(controller.text()).toBe('')
  requests[0]!.resolve('\u8fd0\u884c\u5b9a\u5411\u6d4b\u8bd5')
  await tick()
  expect(controller.text()).toBe('\u8fd0\u884c\u5b9a\u5411\u6d4b\u8bd5')
  expect(controller.take()).toBe('\u8fd0\u884c\u5b9a\u5411\u6d4b\u8bd5')
  expect(controller.text()).toBe('')
  expect(controller.take()).toBe('')
})

test('typing cancels transport and late success cannot reappear', async () => {
  const { controller, requests } = harness()
  controller.start(messages())
  controller.cancel()
  expect(requests[0]!.aborted).toBe(true)
  requests[0]!.resolve('\u8fd0\u884c\u6d4b\u8bd5')
  await tick()
  expect(controller.text()).toBe('')
})

test('session/run changes discard results, including changing back after cancellation', async () => {
  const h = harness()
  h.controller.start(messages())
  h.setKey('other:2')
  h.requests[0]!.resolve('old result')
  await tick()
  expect(h.controller.text()).toBe('')
  h.controller.cancel()
  h.setKey('session:1')
  expect(h.controller.text()).toBe('')
})

test('new request cancels the previous one; older completion never overwrites newest', async () => {
  const h = harness()
  h.controller.start(messages())
  h.setKey('session:2')
  h.controller.start(messages())
  expect(h.requests[0]!.aborted).toBe(true)
  h.requests[1]!.resolve('\u65b0\u7684\u5efa\u8bae')
  await tick()
  h.requests[0]!.resolve('\u8fc7\u671f\u5efa\u8bae')
  await tick()
  expect(h.controller.text()).toBe('\u65b0\u7684\u5efa\u8bae')
})

test('draft, overlay or unavailable capability silently suppresses results', async () => {
  const h = harness()
  h.setEligible(false)
  h.controller.start(messages())
  expect(h.requests).toHaveLength(0)
  h.setEligible(true)
  h.controller.start(messages())
  h.setEligible(false)
  h.requests[0]!.resolve('\u8fc7\u671f\u5efa\u8bae')
  await tick()
  h.setEligible(true)
  expect(h.controller.text()).toBe('')
})

test('asynchronous overlays and attachment changes cancel rather than only hiding a result', async () => {
  const h = harness()
  h.controller.start(messages())
  h.setEligible(false)
  h.controller.reconcile()
  expect(h.requests[0]!.aborted).toBe(true)
  h.requests[0]!.resolve('Run the tests')
  await tick()
  h.setEligible(true)
  expect(h.controller.text()).toBe('')
})

test('errors, nulls, slash commands and control text are silent', async () => {
  for (const text of [null, '', '/new', '\u001b[31mtext', 'first\nsecond', 'a\u202eb']) {
    const h = harness()
    h.controller.start(messages())
    h.requests[0]!.resolve(text)
    await tick()
    expect(h.controller.text()).toBe('')
  }
  const h = harness()
  h.controller.start(messages())
  h.requests[0]!.reject(new Error('unavailable'))
  await tick()
  expect(h.controller.text()).toBe('')
})

test('duplicate state notifications and redraws never restart a request, including after silence', async () => {
  const h = harness()
  h.controller.start(messages(), 'resume')
  h.controller.start(messages(), 'resume')
  expect(h.requests).toHaveLength(1)
  h.requests[0]!.resolve(null)
  await tick()
  h.controller.start(messages(), 'resume')
  expect(h.requests).toHaveLength(1)
  h.controller.start(messages(), 'mode_changed')
  expect(h.requests).toHaveLength(2)
})

test('Codex boundaries apply equally to resume, mode changes and loop waits', () => {
  for (const trigger of ['answer', 'resume', 'mode_changed', 'loop_waiting'] as const) {
    const h = harness()
    h.controller.start(messages().slice(-2), trigger)
    h.controller.start([...messages(), messages()[0]!], trigger)
    expect(h.requests).toHaveLength(0)
    h.controller.start(messages(), trigger)
    expect(h.requests).toHaveLength(1)
  }
})

test('busy input never starts an early speculative sample', async () => {
  const h = harness()
  h.setEligible(false)
  h.controller.start(messages())
  expect(h.requests).toHaveLength(0)
  h.setEligible(true)
  h.controller.start(messages())
  h.requests[0]!.resolve('Run the tests')
  await tick()
  expect(h.controller.text()).toBe('Run the tests')
})

test('local command allowlist governs both receipt and acceptance without model metadata', async () => {
  const guidance = resolveComposerGuidance({ interaction: resolveRunInteraction({ active: false, owner: null, loopParked: true }), mode: 'plan' })
  let actions = guidance.allowedCommands
  const h = harness({ allowsCommand: text => allowedSuggestionCommand(text, actions) })
  h.controller.start(messages(), 'loop_waiting')
  h.requests[0]!.resolve('/loop 15m')
  await tick()
  expect(h.controller.text()).toBe('/loop 15m')
  actions = []
  expect(h.controller.take()).toBe('')
  h.setKey('session:2')
  h.controller.start(messages(), 'loop_waiting')
  h.requests[1]!.resolve('/new')
  await tick()
  expect(h.controller.text()).toBe('')
})

test('commands and incomplete context do not start samples', () => {
  const h = harness()
  h.controller.start([...messages().slice(0, 2), { ...messages()[2]!, text: '/sessions \u5c0f\u7ea2\u4e66' }, messages()[3]!])
  h.controller.start([messages()[0]!])
  expect(h.requests).toHaveLength(0)
})
