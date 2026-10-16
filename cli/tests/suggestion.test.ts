import { expect, test } from 'bun:test'
import { PromptSuggestion, suggestionContext, isSuggestionBoundary, suggestionHistoryFromLines } from '../src/term/app/suggestion.js'
import type { UIMessage } from '../src/term/app/types.js'
import { resolveComposerGuidance, suggestionProductContext, allowedSuggestionCommand, type SuggestionProductContext } from '../src/term/app/composer-guidance.js'
import { resolveRunInteraction } from '../src/term/app/run-interaction.js'

function messages(): UIMessage[] {
  return [
    { id: 'u', role: 'user', text: '\u7ee7\u7eed\u5b9e\u73b0 suggestion \u6d4b\u8bd5', timestamp: 1 },
    { id: 'a', role: 'assistant', text: '', timestamp: 2, content: [
      { type: 'thinking', text: 'private reasoning', contentIndex: 0 },
      { type: 'text', text: 'Tests are ready.', contentIndex: 1 },
    ] },
  ]
}

function harness(options: { canSample?: () => boolean; productContext?: () => SuggestionProductContext; allowsCommand?: (text: string) => boolean } = {}) {
  let key = 'session:1'
  let eligible = true
  let changes = 0
  const requests: { resolve(value: string | null): void; reject(error: Error): void; aborted: boolean; context?: SuggestionProductContext }[] = []
  const controller = new PromptSuggestion({
    key: () => key, eligible: () => eligible, changed: () => changes++,
    ...options,
    start: (_messages, context) => {
      let resolve!: (value: string | null) => void
      let reject!: (error: Error) => void
      const result = new Promise<string | null>((yes, no) => { resolve = yes; reject = no })
      const request = { resolve, reject, aborted: false, context }
      requests.push(request)
      return { result: () => result, abort: () => { request.aborted = true } }
    },
  })
  return { controller, requests, setKey: (value: string) => { key = value },
    setEligible: (value: boolean) => { eligible = value }, changes: () => changes }
}

const tick = () => new Promise<void>(resolve => queueMicrotask(resolve))

test('context contains only recent visible user/assistant prose, not thinking/tools/system', () => {
  const history = messages()
  history.unshift({ id: 's', role: 'system', text: 'secret instructions', timestamp: 0 })
  history.splice(2, 0, { id: 'tool', role: 'assistant', text: 'tool chatter', timestamp: 1, content: [
    { type: 'tool_call', contentIndex: 0, toolCall: { id: 't', name: 'bash', args: { command: 'secret' }, status: 'done' } },
  ] })
  expect(suggestionContext(history)).toEqual([
    { role: 'user', text: '\u7ee7\u7eed\u5b9e\u73b0 suggestion \u6d4b\u8bd5' },
    { role: 'assistant', text: 'Tests are ready.' },
  ])
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
  expect(h.requests).toHaveLength(1)
})

test('context is bounded and keeps the end of long replies', () => {
  const history = Array.from({ length: 30 }, (_, index): UIMessage => ({
    id: String(index), role: index % 2 ? 'assistant' : 'user', timestamp: index,
    text: 'x'.repeat(3000) + '\u7ed3\u8bba',
  }))
  const context = suggestionContext(history)
  expect(context).toHaveLength(6)
  expect(context.every(message => message.text.length <= 1024)).toBe(true)
  expect(context.at(-1)?.text.endsWith('\u7ed3\u8bba')).toBe(true)
})

test('starts at authoritative text-only stop, not per delta or after run teardown', () => {
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

test('resume can sample an interrupted conversation ending with a user message', () => {
  const h = harness()
  h.controller.start([messages()[0]!], 'resume')
  expect(h.requests).toHaveLength(1)
})

test('early final-answer sampling stores a result without showing it while still active', async () => {
  const h = harness({ canSample: () => true })
  h.setEligible(false)
  h.controller.start(messages())
  h.requests[0]!.resolve('Run the tests')
  await tick()
  expect(h.controller.text()).toBe('')
  h.setEligible(true)
  expect(h.controller.text()).toBe('Run the tests')
})

test('current product context and exact command allowlist govern both receipt and Tab', async () => {
  const guidance = resolveComposerGuidance({ interaction: resolveRunInteraction({ active: false, owner: null, loopParked: true }), mode: 'plan' })
  const product = suggestionProductContext(guidance, 'loop_waiting')
  let actions = guidance.actions
  const h = harness({ productContext: () => product, allowsCommand: text => allowedSuggestionCommand(text, actions) })
  h.controller.start(messages(), 'loop_waiting')
  expect(h.requests[0]!.context).toEqual(product)
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
  h.controller.start([{ ...messages()[0]!, text: '/sessions \u5c0f\u7ea2\u4e66' }, messages()[1]!])
  h.controller.start([messages()[0]!])
  expect(h.requests).toHaveLength(0)
})
