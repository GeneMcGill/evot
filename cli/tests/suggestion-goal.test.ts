import { expect, test } from 'bun:test'
import { SuggestionGoal } from '../src/term/app/suggestion-goal.js'
import { suggestionContext } from '../src/term/app/suggestion.js'
import { COMPACT_SUMMARY_PREFIX } from '../src/term/app/resume.js'
import type { UIMessage } from '../src/term/app/types.js'

const message = (text: string, role: UIMessage['role'] = 'user'): UIMessage => ({ id: text, role, text, timestamp: 0 })

test('first request survives long recent history, window trimming, and later corrections', () => {
  const goal = new SuggestionGoal()
  goal.observe('Implement login with audit events')
  const history: UIMessage[] = []
  for (let index = 0; index < 20; index++) {
    goal.observe(`Refinement ${index}`)
    history.push(message(`Refinement ${index}`), message(`Done ${index}`, 'assistant'))
  }
  expect(suggestionContext(history).some(item => item.text.includes('audit events'))).toBe(false)
  expect(goal.value()).toBe('Implement login with audit events')
})

test('restore captures the full transcript goal before the display tail is selected', () => {
  const goal = new SuggestionGoal()
  const history = [message('Initial login requirements'), ...Array.from({ length: 80 }, (_, i) => message(`Follow-up ${i}`))]
  goal.restore(history)
  expect(history.slice(-20).some(item => item.text === goal.value())).toBe(false)
  expect(goal.value()).toBe('Initial login requirements')
})

test('summary boundaries and system or assistant messages are never the original task', () => {
  const goal = new SuggestionGoal()
  goal.restore([message('Internal state', 'system'), message('Earlier answer', 'assistant'),
    message(`${COMPACT_SUMMARY_PREFIX} has been compacted`), message('Add login tests')])
  expect(goal.value()).toBe('Add login tests')
})

test('restore and explicit reset cannot leak another session goal', () => {
  const goal = new SuggestionGoal()
  goal.observe('Session A goal')
  goal.restore([message('Session B goal')])
  expect(goal.value()).toBe('Session B goal')
  goal.restore([])
  expect(goal.value()).toBeUndefined()
  goal.observe('New goal after clear')
  goal.reset()
  expect(goal.value()).toBeUndefined()
})

test('the goal is Unicode-safe, bounded, and retains the final requirement', () => {
  const goal = new SuggestionGoal()
  goal.observe(`Build login ${'\u{1f680}'.repeat(1000)} retain audit events`)
  const value = goal.value()!
  expect(Array.from(value)).toHaveLength(512)
  expect(value.startsWith('Build login ')).toBe(true)
  expect(value.endsWith('retain audit events')).toBe(true)
  expect(value).not.toContain('\ufffd')
})
