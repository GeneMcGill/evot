import chalk from 'chalk'
import { getTheme } from '../render/theme/index.js'
import { formatDuration } from './parser.js'

/** Under a minute to go the row turns yellow: the agent is about to wake. */
export const LOOP_WAKE_SOON_MS = 60_000

export interface LoopWaitLineInput {
  /** Milliseconds until the scheduled wakeup. */
  remainingMs: number
  /** How long a background task the agent is watching has been running, if
   *  one is; it may wake the agent before the timer does. */
  backgroundElapsedMs?: number
}

/**
 * The status row above the composer while a loop is parked between ticks.
 * It takes the slot the run spinner uses while busy, because it answers the
 * same question, "what is the agent doing right now": waiting, until when,
 * on what. How to act on it (type, change pace, esc) is the composer
 * placeholder's job, right beneath. When a background watcher is also
 * running the two waits are one state, so they share the row instead of
 * stacking.
 */
export function formatLoopWaitLine(input: LoopWaitLineInput): string {
  const parts = [`wakes in ${formatDuration(input.remainingMs)}`]
  if (input.backgroundElapsedMs !== undefined) {
    parts.push(`watching a background task (${formatDuration(input.backgroundElapsedMs)})`)
  }
  const soon = input.remainingMs <= LOOP_WAKE_SOON_MS
  const tone = soon ? chalk.yellow : chalk.hex(getTheme().brandHex)
  return tone(`◌ loop · ${parts.join(' · ')}`)
}
