import type { HostToolExtension } from '../term/host-tools.js'
import { LoopController, type LoopSnapshot, type RunOutcome, type StopCause } from './controller.js'
import { createLoopExtension } from './host-tool.js'
import { formatDuration, parseLoopArgs, type LoopCommand } from './parser.js'
import { buildLoopStartPrompt, buildLoopWakeupPrompt } from './prompt.js'
import { formatWallClock as clock } from '../render/format.js'

export interface LoopSessionDeps {
  runTurn: (userLine: string | null, prompt: string, extension: HostToolExtension) => void
  canWake: () => boolean
  notify: (text: string) => void
  requestRender: () => void
  waitingChanged?: (waiting: boolean) => void
}

const TICK_MS = 1000

const STOP_TEXT: Record<StopCause, string> = {
  user: 'stopped by you',
  model: 'done · agent reported the goal reached',
  max_age: 'stopped · 72h limit reached',
  errors: 'stopped · 3 consecutive run errors',
  no_reschedule: 'stopped · agent did not schedule a wakeup twice',
  session_switch: 'stopped · session changed',
}


export class LoopSession {
  readonly #controller: LoopController
  readonly #extension: HostToolExtension
  readonly #deps: LoopSessionDeps
  #timer: ReturnType<typeof setInterval> | null = null
  #runBegun = false
  #pendingStopNotice: string | null = null
  #waitingReported = false

  constructor(deps: LoopSessionDeps) {
    this.#deps = deps
    this.#controller = new LoopController({
      stopped: (cause, snapshot) => this.#onStopped(cause, snapshot),
    })
    this.#extension = createLoopExtension(this.#controller)
  }

  get active(): boolean {
    return this.#controller.active
  }

  get snapshot(): LoopSnapshot | null {
    return this.#controller.snapshot
  }

  get waiting(): boolean {
    return this.#controller.remainingMs() !== null
  }

  get extension(): HostToolExtension | undefined {
    return this.#controller.isDynamic ? this.#extension : undefined
  }

  handleCommand(rawInput: string, args: string): void {
    const command: LoopCommand = parseLoopArgs(args)
    switch (command.kind) {
      case 'invalid':
        this.#deps.notify(command.reason)
        return
      case 'stop':
        if (!this.#controller.stop('user')) this.#deps.notify('No loop is running.')
        return
      case 'status':
        this.#deps.notify(this.statusLine() ?? 'No loop is running. Usage: /loop [interval] <prompt>')
        return
      case 'start': {
        if (this.#controller.active) this.#controller.stop('user')
        this.#controller.start(command.mode, command.prompt)
        this.#ensureTimer()
        this.#startRun()
        this.#deps.runTurn(rawInput, buildLoopStartPrompt(command.mode, command.prompt), this.#extension)
        return
      }
    }
  }

  beginRun(): void {
    if (this.#runBegun) return
    this.#startRun()
  }

  endRun(outcome: RunOutcome): void {
    this.#runBegun = false
    this.#controller.endRun(outcome)
    this.#flushStopNotice()
    this.#syncWaiting()
    this.#deps.requestRender()
  }

  reset(cause: StopCause = 'session_switch'): void {
    this.#controller.stop(cause)
  }

  statusLine(now = Date.now()): string | null {
    const snapshot = this.#controller.snapshot
    if (!snapshot) return null
    const remaining = this.#controller.remainingMs(now)
    const mode = snapshot.mode.kind === 'fixed' ? `every ${formatDuration(snapshot.mode.intervalMs)}` : 'self-paced'
    const next = remaining === null ? 'running now' : `next wakeup in ${formatDuration(remaining)} (${clock(now + remaining)})`
    const noop = snapshot.noopStreak > 0 ? ` · ${snapshot.noopStreak} quiet` : ''
    const reason = snapshot.reason ? `\n  ${snapshot.reason}` : ''
    return `◌ loop · ${mode} · tick ${snapshot.tick} · ${next}${noop} · since ${clock(snapshot.startedAt)}${reason}`
  }

  waitLine(now = Date.now()): string | null {
    const remaining = this.#controller.remainingMs(now)
    if (remaining === null) return null
    return `◌ loop · next wakeup in ${formatDuration(remaining)} (${clock(now + remaining)})`
  }

  dispose(): void {
    this.#clearTimer()
  }

  #ensureTimer(): void {
    if (this.#timer) return
    this.#timer = setInterval(() => this.#tick(), TICK_MS)
    ;(this.#timer as unknown as { unref?: () => void }).unref?.()
  }

  #clearTimer(): void {
    if (!this.#timer) return
    clearInterval(this.#timer)
    this.#timer = null
  }

  #tick(): void {
    if (!this.#controller.active) {
      this.#clearTimer()
      return
    }
    this.#deps.requestRender()
    if (!this.#controller.due() || !this.#deps.canWake()) return
    this.#startRun()
    const snapshot = this.#controller.snapshot
    if (!snapshot) return
    const now = Date.now()
    const quiet = snapshot.noopStreak > 0 ? ` · ${snapshot.noopStreak} quiet tick${snapshot.noopStreak === 1 ? '' : 's'} so far` : ''
    this.#deps.notify(`◌ loop wakeup #${snapshot.tick} (${clock(now)})${quiet}`)
    this.#deps.runTurn(null, buildLoopWakeupPrompt(snapshot, now), this.#extension)
  }

  #startRun(): void {
    this.#runBegun = true
    this.#controller.beginRun()
    this.#syncWaiting()
  }

  #syncWaiting(): void {
    const waiting = !this.#runBegun && this.waiting
    if (waiting === this.#waitingReported) return
    this.#waitingReported = waiting
    this.#deps.waitingChanged?.(waiting)
  }

  #onStopped(cause: StopCause, snapshot: LoopSnapshot): void {
    this.#clearTimer()
    const ticks = `${snapshot.tick} tick${snapshot.tick === 1 ? '' : 's'}`
    const ran = formatDuration(Date.now() - snapshot.startedAt)
    const notice = `⏹ loop ${STOP_TEXT[cause]} · ${ticks} · ran ${ran}`
    if (this.#runBegun) {
      this.#pendingStopNotice = notice
      return
    }
    this.#deps.notify(notice)
    this.#syncWaiting()
    this.#deps.requestRender()
  }

  #flushStopNotice(): void {
    const notice = this.#pendingStopNotice
    if (!notice) return
    this.#pendingStopNotice = null
    this.#deps.notify(notice)
  }
}
