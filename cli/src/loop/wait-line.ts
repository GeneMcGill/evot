import chalk from 'chalk'
import { getTheme } from '../render/theme/index.js'
import { formatDuration } from './parser.js'

/** Under a minute to go the row turns yellow: the agent is about to wake. */
export const LOOP_WAKE_SOON_MS = 60_000

export interface LoopWaitLineInput {
  /** Milliseconds until the scheduled wakeup. */
  remainingMs: number
  /** A first Esc has armed the stop; the row asks for the confirming press. */
  stopPending?: boolean
}

/**
 * The status row above the composer while a loop is parked between ticks.
 * It takes the slot the run spinner uses while busy, because it answers the
 * same question, "what is the agent doing right now": waiting, until when.
 * How to stop it lives in the composer placeholder; a background shell the
 * agent left running lives in the footer chip. One fact per place.
 */
export function formatLoopWaitLine(input: LoopWaitLineInput): string {
  const soon = input.remainingMs <= LOOP_WAKE_SOON_MS
  const tone = soon ? chalk.yellow : chalk.hex(getTheme().brandHex)
  const line = tone(`◌ loop · wakes in ${formatDuration(input.remainingMs)}`)
  // The confirmation prompt is the one moment the row talks about keys: the
  // placeholder said "esc twice", the first press happened, say what is next.
  return input.stopPending ? `${line}${chalk.yellow(' · esc again to stop the loop')}` : line
}
