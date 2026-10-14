import { describe, expect, test } from 'bun:test'
import { parseLoopArgs, parseDuration, formatDuration } from '../src/loop/parser.js'
import {
  LoopController,
  clampDelaySeconds,
  KEEPALIVE_SECONDS,
  MAX_AGE_MS,
  MAX_CONSECUTIVE_ERRORS,
  type StopCause,
} from '../src/loop/controller.js'
import { createLoopExtension, SCHEDULE_WAKEUP_SPEC } from '../src/loop/host-tool.js'
import { buildLoopStartPrompt, buildLoopWakeupPrompt } from '../src/loop/prompt.js'
import type { HostToolExtension } from '../src/term/host-tools.js'
import { LoopSession } from '../src/loop/session.js'
import { composeExtensions, type HostToolCall, type HostToolExtension } from '../src/term/host-tools.js'

function controller() {
  const stops: StopCause[] = []
  const c = new LoopController({ stopped: cause => stops.push(cause) })
  return { c, stops }
}

function call(args: Record<string, unknown>, name = 'schedule_wakeup'): HostToolCall {
  return { tool_name: name, tool_call_id: 'c1', arguments: args }
}

describe('parseLoopArgs', () => {
  test('control words', () => {
    expect(parseLoopArgs('stop')).toEqual({ kind: 'stop' })
    expect(parseLoopArgs(' STATUS ')).toEqual({ kind: 'status' })
  })

  test('leading interval', () => {
    expect(parseLoopArgs('5m check CI')).toEqual({ kind: 'start', mode: { kind: 'fixed', intervalMs: 300_000 }, prompt: 'check CI' })
  })

  test('trailing interval', () => {
    expect(parseLoopArgs('check CI every 20m')).toEqual({ kind: 'start', mode: { kind: 'fixed', intervalMs: 1_200_000 }, prompt: 'check CI' })
  })

  test('no interval is dynamic', () => {
    expect(parseLoopArgs('watch PR 123')).toEqual({ kind: 'start', mode: { kind: 'dynamic' }, prompt: 'watch PR 123' })
  })

  test('rejects empty prompt and out-of-range interval', () => {
    expect(parseLoopArgs('').kind).toBe('status')
    expect(parseLoopArgs('5m').kind).toBe('invalid')
    expect(parseLoopArgs('10s x').kind).toBe('invalid')
    expect(parseLoopArgs('2h x').kind).toBe('invalid')
  })

  test('durations', () => {
    expect(parseDuration('90s')).toBe(90_000)
    expect(parseDuration('1h')).toBe(3_600_000)
    expect(parseDuration('abc')).toBeNull()
    expect(formatDuration(90_000)).toBe('1m30s')
    expect(formatDuration(3_600_000)).toBe('1h')
  })
})

describe('clampDelaySeconds', () => {
  test('bounds and cache window', () => {
    expect(clampDelaySeconds(10)).toBe(60)
    expect(clampDelaySeconds(99_999)).toBe(3600)
    expect(clampDelaySeconds(300)).toBe(270)
    expect(clampDelaySeconds(329)).toBe(270)
    expect(clampDelaySeconds(331)).toBe(331)
    expect(clampDelaySeconds(269)).toBe(269)
  })
})

describe('LoopController', () => {
  test('fixed mode advances nextFireAt at run end', () => {
    const { c } = controller()
    c.start({ kind: 'fixed', intervalMs: 60_000 }, 'p', 1000)
    c.beginRun(1000)
    c.endRun('ok', 5000)
    expect(c.remainingMs(5000)).toBe(60_000)
    expect(c.due(65_000)).toBe(true)
  })

  test('dynamic schedule overrides and counts noop streak', () => {
    const { c } = controller()
    c.start({ kind: 'dynamic' }, 'p', 0)
    c.beginRun(0)
    expect(c.schedule({ delaySeconds: 120, reason: 'a', noop: true }, 0)).toBe(120)
    c.endRun('ok', 1)
    expect(c.snapshot?.noopStreak).toBe(1)
    expect(c.remainingMs(1)).toBe(119_999)
    c.beginRun(120_000)
    c.schedule({ delaySeconds: 60, reason: 'b', noop: false }, 120_000)
    expect(c.snapshot?.noopStreak).toBe(0)
  })

  test('keepalive once, then stop', () => {
    const { c, stops } = controller()
    c.start({ kind: 'dynamic' }, 'p', 0)
    c.beginRun(0)
    c.endRun('ok', 0)
    expect(c.active).toBe(true)
    expect(c.remainingMs(0)).toBe(KEEPALIVE_SECONDS * 1000)
    c.beginRun(1)
    c.endRun('ok', 1)
    expect(c.active).toBe(false)
    expect(stops).toEqual(['no_reschedule'])
  })

  test('consecutive errors stop the loop', () => {
    const { c, stops } = controller()
    c.start({ kind: 'fixed', intervalMs: 60_000 }, 'p', 0)
    for (let i = 0; i < MAX_CONSECUTIVE_ERRORS; i++) {
      c.beginRun(i)
      c.endRun('error', i)
    }
    expect(stops).toEqual(['errors'])
  })

  test('error counter resets on success', () => {
    const { c } = controller()
    c.start({ kind: 'fixed', intervalMs: 60_000 }, 'p', 0)
    c.beginRun(0); c.endRun('error', 0)
    c.beginRun(1); c.endRun('error', 1)
    c.beginRun(2); c.endRun('ok', 2)
    c.beginRun(3); c.endRun('error', 3)
    expect(c.active).toBe(true)
  })

  test('max age stops at run start', () => {
    const { c, stops } = controller()
    c.start({ kind: 'dynamic' }, 'p', 0)
    c.beginRun(MAX_AGE_MS + 1)
    expect(stops).toEqual(['max_age'])
  })

  test('stop is idempotent', () => {
    const { c, stops } = controller()
    c.start({ kind: 'dynamic' }, 'p', 0)
    expect(c.stop('user')).not.toBeNull()
    expect(c.stop('user')).toBeNull()
    expect(stops).toEqual(['user'])
  })
})

describe('schedule_wakeup host tool', () => {
  test('spec aliases ScheduleWakeup', () => {
    expect(SCHEDULE_WAKEUP_SPEC.name_aliases).toEqual([['claude', 'ScheduleWakeup']])
    const { c } = controller()
    const ext = createLoopExtension(c)
    expect(ext.handles('ScheduleWakeup')).toBe(true)
    expect(ext.handles('ask_user')).toBe(false)
  })

  test('requires delay unless stop', async () => {
    const { c } = controller()
    c.start({ kind: 'dynamic' }, 'p', 0)
    const ext = createLoopExtension(c)
    const missing = await ext.dispatch(call({ reason: 'x' }))
    expect(missing.is_error).toBe(true)
    const ok = await ext.dispatch(call({ delay_seconds: 600, reason: 'waiting on CI', noop: false }))
    expect(ok.is_error).toBe(false)
    expect(ok.content[0]?.text).toContain('10m')
    expect(c.snapshot?.reason).toBe('waiting on CI')
  })

  test('stop ends the loop', async () => {
    const { c, stops } = controller()
    c.start({ kind: 'dynamic' }, 'p', 0)
    const ext = createLoopExtension(c)
    const res = await ext.dispatch(call({ stop: true, reason: 'converged' }))
    expect(res.is_error).toBe(false)
    expect(c.active).toBe(false)
    expect(stops).toEqual(['model'])
  })

  test('errors when no loop is active', async () => {
    const { c } = controller()
    const res = await createLoopExtension(c).dispatch(call({ delay_seconds: 60, reason: 'x' }))
    expect(res.is_error).toBe(true)
  })
})

describe('composeExtensions', () => {
  const a: HostToolExtension = {
    specsJson: JSON.stringify([{ name: 'ask_user' }, { name: 'a' }]),
    handles: n => n === 'a',
    dispatch: async c => ({ tool_call_id: c.tool_call_id, content: [{ type: 'text', text: 'A' }], is_error: false }),
  }
  const b: HostToolExtension = {
    specsJson: JSON.stringify([{ name: 'ask_user' }, { name: 'b' }]),
    handles: n => n === 'b',
    dispatch: async c => ({ tool_call_id: c.tool_call_id, content: [{ type: 'text', text: 'B' }], is_error: false }),
  }

  test('passthrough', () => {
    expect(composeExtensions()).toBeUndefined()
    expect(composeExtensions(undefined, a)).toBe(a)
  })

  test('merges specs without duplicates and routes dispatch', async () => {
    const merged = composeExtensions(a, b)!
    expect((JSON.parse(merged.specsJson) as Array<{ name: string }>).map(s => s.name)).toEqual(['ask_user', 'a', 'b'])
    expect(merged.handles('a')).toBe(true)
    expect(merged.handles('b')).toBe(true)
    expect(merged.handles('c')).toBe(false)
    expect((await merged.dispatch(call({}, 'b'))).content[0]?.text).toBe('B')
    expect((await merged.dispatch(call({}, 'c'))).is_error).toBe(true)
  })
})

describe('prompts', () => {
  test('dynamic start mentions schedule_wakeup, fixed does not', () => {
    expect(buildLoopStartPrompt({ kind: 'dynamic' }, 'watch')).toContain('schedule_wakeup')
    expect(buildLoopStartPrompt({ kind: 'fixed', intervalMs: 300_000 }, 'watch')).not.toContain('schedule_wakeup')
    expect(buildLoopStartPrompt({ kind: 'fixed', intervalMs: 300_000 }, 'watch')).toContain('every 5m')
  })

  test('wakeup prompt is short and tick-numbered', () => {
    const text = buildLoopWakeupPrompt({
      active: true, mode: { kind: 'dynamic' }, prompt: 'x'.repeat(200), startedAt: 0,
      nextFireAt: null, reason: null, tick: 7, noopStreak: 0,
    })
    expect(text).toContain('#7')
    expect(text).toContain('…')
    expect(text).toContain('schedule_wakeup')
    expect(text.length).toBeLessThan(220)
  })
})

describe('LoopSession', () => {
  function session(canWake = true) {
    const turns: Array<{ userLine: string | null; prompt: string }> = []
    const notices: string[] = []
    const s = new LoopSession({
      runTurn: (userLine, prompt) => turns.push({ userLine, prompt }),
      canWake: () => canWake,
      notify: text => notices.push(text),
      requestRender: () => {},
    })
    return { s, turns, notices }
  }

  test('start runs first turn with the user line', () => {
    const { s, turns } = session()
    s.handleCommand('/loop watch PR', 'watch PR')
    expect(s.active).toBe(true)
    expect(turns).toHaveLength(1)
    expect(turns[0]?.userLine).toBe('/loop watch PR')
    expect(turns[0]?.prompt).toContain('watch PR')
    s.dispose()
  })

  test('dynamic exposes extension, fixed does not', () => {
    const { s } = session()
    s.handleCommand('/loop a', 'a')
    expect(s.extension).toBeDefined()
    s.handleCommand('/loop 5m b', '5m b')
    expect(s.extension).toBeUndefined()
    s.dispose()
  })

  test('stop and status notices', () => {
    const { s, notices } = session()
    s.handleCommand('/loop status', 'status')
    expect(notices.at(-1)).toContain('No loop is running.')
    s.handleCommand('/loop x', 'x')
    s.handleCommand('/loop status', 'status')
    expect(notices.at(-1)).toContain('self-paced')
    s.endRun('ok')
    s.handleCommand('/loop stop', 'stop')
    expect(s.active).toBe(false)
    expect(notices.at(-1)).toContain('loop stopped')
    s.dispose()
  })

  test('waitRemaining is the countdown between ticks and null while running', () => {
    const { s } = session()
    const t0 = Date.now()
    s.handleCommand('/loop 20m x', '20m x')
    expect(s.waitRemaining(t0)).toBeNull()
    s.endRun('ok')
    expect(s.waiting).toBe(true)
    const wait = s.waitRemaining(t0 + 23_000)
    expect(wait?.label).toMatch(/^19m3\ds$/)
    expect(wait?.remainingMs).toBeGreaterThan(19 * 60_000)
    s.dispose()
  })

  test('endRun error streak stops with notice', () => {
    const { s, notices } = session()
    s.handleCommand('/loop 1m x', '1m x')
    s.endRun('error')
    s.beginRun(); s.endRun('error')
    s.beginRun(); s.endRun('error')
    expect(s.active).toBe(false)
    expect(notices.at(-1)).toContain('consecutive run errors')
    s.dispose()
  })
})

describe('loop tool card', () => {
  const base = { id: 't1', name: 'schedule_wakeup', startedAt: Date.UTC(2026, 0, 1, 12, 0, 0) }

  test('scheduled wakeup shows countdown, clock and reason', async () => {
    const { buildToolCard } = await import('../src/render/output.js')
    const lines = buildToolCard({
      ...base, status: 'done',
      args: { delay_seconds: 1600, reason: 'Waiting for the PR watcher to exit on a new codex review.', noop: false },
    })
    expect(lines).toHaveLength(2)
    expect(lines[0]?.text).toMatch(/^◌ loop · next wakeup in 26m 40s \(\d\d:\d\d:\d\d\)$/)
    expect(lines[1]?.text).toBe('  Waiting for the PR watcher to exit on a new codex review.')
    expect(lines[0]?.toolCard?.state).toBe('success')
  })

  test('quiet tick is marked', async () => {
    const { buildToolCard } = await import('../src/render/output.js')
    const [head] = buildToolCard({ ...base, status: 'done', args: { delay_seconds: 60, reason: 'r', noop: true } })
    expect(head?.text).toContain('◌ loop · quiet · next wakeup in 1m')
  })

  test('stop and error states', async () => {
    const { buildToolCard } = await import('../src/render/output.js')
    expect(buildToolCard({ ...base, status: 'done', args: { stop: true, reason: 'converged' } })[0]?.text).toBe('⏹ loop · stopping')
    const err = buildToolCard({ ...base, status: 'error', args: { reason: 'x' }, result: 'delay_seconds is required unless stop is true.' })
    expect(err[0]?.text).toBe('✗ loop · wakeup not scheduled')
    expect(err[1]?.text).toContain('delay_seconds is required')
    expect(buildToolCard({ ...base, status: 'running', args: {} })[0]?.text).toBe('◌ loop · scheduling next wakeup…')
  })
})

describe('LoopSession stop notice ordering', () => {
  test('a stop raised during a run is reported at endRun, not mid-stream', async () => {
    const { LoopSession } = await import('../src/loop/session.js')
    const notices: string[] = []
    let extension: HostToolExtension | undefined
    const session = new LoopSession({
      runTurn: (_line, _prompt, ext) => { extension = ext },
      canWake: () => true,
      notify: text => notices.push(text),
      requestRender: () => {},
    })
    session.handleCommand('/loop watch', 'watch')
    if (!extension) throw new Error('no extension')
    await extension.dispatch({ tool_call_id: '1', tool_name: 'schedule_wakeup', arguments: { stop: true, reason: 'converged' } })
    expect(session.active).toBe(false)
    expect(notices).toHaveLength(0)
    session.endRun('ok')
    expect(notices).toHaveLength(1)
    expect(notices[0]).toContain('⏹ loop done')
  })

  test('a user stop while idle is reported immediately', async () => {
    const { LoopSession } = await import('../src/loop/session.js')
    const notices: string[] = []
    const session = new LoopSession({
      runTurn: () => {},
      canWake: () => true,
      notify: text => notices.push(text),
      requestRender: () => {},
    })
    session.handleCommand('/loop watch', 'watch')
    session.endRun('ok')
    session.handleCommand('/loop stop', 'stop')
    expect(notices.at(-1)).toContain('⏹ loop stopped by you')
  })
})

describe('LoopSession waiting state', () => {
  test('reports working while a wakeup is pending and idle once stopped', async () => {
    const { LoopSession } = await import('../src/loop/session.js')
    const transitions: boolean[] = []
    let extension: HostToolExtension | undefined
    const session = new LoopSession({
      runTurn: (_line, _prompt, ext) => { extension = ext },
      canWake: () => true,
      notify: () => {},
      requestRender: () => {},
      waitingChanged: w => transitions.push(w),
    })
    session.handleCommand('/loop watch', 'watch')
    expect(session.waiting).toBe(false)
    if (!extension) throw new Error('no extension')
    await extension.dispatch({ tool_call_id: '1', tool_name: 'schedule_wakeup', arguments: { delay_seconds: 600, reason: 'r' } })
    expect(transitions).toEqual([])
    session.endRun('ok')
    expect(session.waiting).toBe(true)
    expect(transitions).toEqual([true])
    session.handleCommand('/loop stop', 'stop')
    expect(transitions).toEqual([true, false])
    session.dispose()
  })
})
