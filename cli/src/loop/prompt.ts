import type { LoopMode } from './parser.js'
import { formatDuration } from './parser.js'
import type { LoopSnapshot } from './controller.js'
import { MIN_DELAY_SECONDS, MAX_DELAY_SECONDS } from './controller.js'

const PROMPT_EXCERPT_CHARS = 120

const SHARED_RULES = [
  'Prefer event-driven waiting over polling: when something external must happen (CI, a review, a deploy), start one background shell command that blocks until the condition changes and exits, so its completion wakes you immediately.',
  'Never busy-poll with short wakeups while a background watcher is running.',
  'When nothing actionable happened, reply in one short line; do not restate the checks you performed.',
  'Keep a running cursor (last review id, last commit, last timestamp) in your reply so later wakeups can compare against it.',
]

export function buildLoopStartPrompt(mode: LoopMode, prompt: string): string {
  const header = mode.kind === 'fixed'
    ? `You are running a recurring loop. The host will wake you every ${formatDuration(mode.intervalMs)} with a short reminder; you do not schedule anything yourself.`
    : 'You are running a self-paced loop. Every turn: do the work, write a short status line (with your cursor), then call `schedule_wakeup` exactly once as the final action; the turn ends when it returns. Call it with `stop: true` when the goal is reached or cannot progress.'

  const dynamicRules = mode.kind === 'dynamic'
    ? [
        `\`delay_seconds\` is clamped to [${MIN_DELAY_SECONDS}, ${MAX_DELAY_SECONDS}]. Use short delays only when you expect a change soon and have no watcher; use 1200-1800 as a heartbeat when a background watcher is running.`,
        'Set `noop: true` when this turn changed nothing and found nothing new; set `noop: false` when you edited files, pushed, commented, or surfaced a finding.',
        'The `reason` must be one specific sentence about what you are waiting for.',
      ]
    : []

  return [
    header,
    '',
    'Task:',
    prompt,
    '',
    'Rules:',
    ...[...SHARED_RULES, ...dynamicRules].map(rule => `- ${rule}`),
    '',
    'Start now: perform the first pass of the task.',
  ].join('\n')
}

export function buildLoopWakeupPrompt(snapshot: LoopSnapshot, now = Date.now()): string {
  const time = new Date(now).toTimeString().slice(0, 5)
  const excerpt = snapshot.prompt.length > PROMPT_EXCERPT_CHARS
    ? `${snapshot.prompt.slice(0, PROMPT_EXCERPT_CHARS)}…`
    : snapshot.prompt
  const tail = snapshot.mode.kind === 'dynamic'
    ? ' End with schedule_wakeup.'
    : ''
  return `/loop wakeup #${snapshot.tick} (${time}). Continue: ${excerpt}${tail}`
}
