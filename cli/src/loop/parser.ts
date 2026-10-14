export type LoopMode =
  | { kind: 'fixed'; intervalMs: number }
  | { kind: 'dynamic' }

export type LoopCommand =
  | { kind: 'stop' }
  | { kind: 'status' }
  | { kind: 'start'; mode: LoopMode; prompt: string }
  | { kind: 'invalid'; reason: string }

export const MIN_INTERVAL_MS = 60_000
export const MAX_INTERVAL_MS = 3_600_000

const UNIT_MS: Record<string, number> = { s: 1_000, m: 60_000, h: 3_600_000 }
const LEADING = /^(\d+)(s|m|h)\b\s*/i
const TRAILING = /\s*\bevery\s+(\d+)(s|m|h)\s*$/i

export function parseDuration(text: string): number | null {
  const match = /^(\d+)(s|m|h)$/i.exec(text.trim())
  if (!match) return null
  const unit = UNIT_MS[match[2]!.toLowerCase()]
  if (unit === undefined) return null
  return Number(match[1]) * unit
}

export function parseLoopArgs(args: string): LoopCommand {
  const trimmed = args.trim()
  const lower = trimmed.toLowerCase()
  if (lower === 'stop' || lower === 'cancel') return { kind: 'stop' }
  if (lower === 'status') return { kind: 'status' }

  let prompt = trimmed
  let intervalMs: number | null = null

  const leading = LEADING.exec(prompt)
  if (leading) {
    intervalMs = parseDuration(`${leading[1]}${leading[2]}`)
    prompt = prompt.slice(leading[0].length)
  } else {
    const trailing = TRAILING.exec(prompt)
    if (trailing) {
      intervalMs = parseDuration(`${trailing[1]}${trailing[2]}`)
      prompt = prompt.slice(0, trailing.index)
    }
  }

  prompt = prompt.trim()
  if (!prompt) return intervalMs === null ? { kind: 'status' } : { kind: 'invalid', reason: 'Usage: /loop [interval] <prompt>' }

  if (intervalMs === null) return { kind: 'start', mode: { kind: 'dynamic' }, prompt }
  if (intervalMs < MIN_INTERVAL_MS || intervalMs > MAX_INTERVAL_MS) {
    return { kind: 'invalid', reason: 'Interval must be between 1m and 1h.' }
  }
  return { kind: 'start', mode: { kind: 'fixed', intervalMs }, prompt }
}

export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000))
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  if (h > 0) return m > 0 ? `${h}h${m}m` : `${h}h`
  if (m > 0) return s > 0 ? `${m}m${s}s` : `${m}m`
  return `${s}s`
}
