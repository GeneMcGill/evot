import {
  ASK_USER_SPEC,
  errorResponse,
  textResponse,
  type HostToolCall,
  type HostToolExtension,
  type HostToolResponse,
} from '../term/host-tools.js'
import type { LoopController } from './controller.js'
import { MIN_DELAY_SECONDS, MAX_DELAY_SECONDS } from './controller.js'
import { formatDuration } from './parser.js'
import { formatClock24 } from '../render/format.js'

export const SCHEDULE_WAKEUP_SPEC = {
  name: 'schedule_wakeup',
  label: 'Schedule Wakeup',
  description: `Schedule the next wakeup of the current /loop, or stop it. Write your status summary first, then call this exactly once as the final action of the turn; the turn ends when it returns. Prefer a long heartbeat (1200-1800s) whenever a background watcher is running; it will wake you the moment it exits. Pass stop: true when the loop's goal is reached or cannot progress.`,
  parameters_schema: {
    type: 'object',
    additionalProperties: false,
    properties: {
      delay_seconds: {
        type: 'integer',
        minimum: MIN_DELAY_SECONDS,
        maximum: MAX_DELAY_SECONDS,
        description: `Seconds until the next wakeup (${MIN_DELAY_SECONDS}-${MAX_DELAY_SECONDS}). Required unless stop is true.`,
      },
      reason: {
        type: 'string',
        description: 'One specific sentence: what you are waiting for and why this delay.',
      },
      noop: {
        type: 'boolean',
        description: 'true when this turn changed nothing and found nothing new.',
      },
      stop: {
        type: 'boolean',
        description: 'true to end the loop now.',
      },
    },
    required: ['reason'],
  },
  name_aliases: [['claude', 'ScheduleWakeup']],
  ends_turn: true,
}

export const LOOP_HOST_TOOL_SPECS_JSON = JSON.stringify([ASK_USER_SPEC, SCHEDULE_WAKEUP_SPEC])

export function isScheduleWakeupName(name: string): boolean {
  const lower = name.toLowerCase()
  return lower === SCHEDULE_WAKEUP_SPEC.name || lower === 'schedulewakeup'
}

export function createLoopExtension(controller: LoopController): HostToolExtension {
  return {
    specsJson: LOOP_HOST_TOOL_SPECS_JSON,
    handles: isScheduleWakeupName,
    dispatch: call => Promise.resolve(dispatchScheduleWakeup(call, controller)),
  }
}

function dispatchScheduleWakeup(call: HostToolCall, controller: LoopController): HostToolResponse {
  if (!controller.active) return errorResponse(call, 'No loop is running.')
  const args = call.arguments
  const reason = typeof args.reason === 'string' ? args.reason.trim() : ''
  if (args.stop === true) {
    controller.stop('model')
    return textResponse(call, `Loop stopped${reason ? `: ${reason}` : '.'}`)
  }
  const delay = typeof args.delay_seconds === 'number' ? args.delay_seconds : NaN
  if (!Number.isFinite(delay)) {
    return errorResponse(call, 'delay_seconds is required unless stop is true.')
  }
  if (!reason) return errorResponse(call, 'reason is required.')
  const applied = controller.schedule({ delaySeconds: delay, reason, noop: args.noop === true })
  const at = formatClock24(Date.now() + applied * 1000)
  const clamped = applied !== Math.round(delay) ? ` (adjusted from ${Math.round(delay)}s)` : ''
  return textResponse(call, `Next wakeup in ${formatDuration(applied * 1000)} at ${at}${clamped}.`)
}
