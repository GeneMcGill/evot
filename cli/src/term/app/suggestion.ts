import type { NativeSuggestion } from '../../native/contracts/ports.js'
import type { UIMessage } from './types.js'
import type { OutputLine } from '../../render/output.js'
import type { SuggestionTrigger } from './composer-guidance.js'

export interface SuggestionMessage { role: 'user' | 'assistant'; text: string }

const HISTORY_TOKENS = 4096

function historyTokens(message: SuggestionMessage): number {
  return Math.ceil(JSON.stringify(message).length / 4)
}

/** Whole recent messages only. Stop at the first oversized message, like
 * Codex; do not cut messages or fill gaps with unrelated older requests. */
export function suggestionContext(messages: readonly UIMessage[]): SuggestionMessage[] {
  const recent: SuggestionMessage[] = []
  let remaining = HISTORY_TOKENS
  for (let i = messages.length - 1; i >= 0; i--) {
    const message = messages[i]!
    if (message.role !== 'user' && message.role !== 'assistant') continue
    if (message.content?.some(block => block.type === 'tool_call')) break
    const raw = message.role === 'assistant' && message.content
      ? message.content.filter(block => block.type === 'text').map(block => block.text).join('\n')
      : message.text
    if (!raw.trim() || raw.length > HISTORY_TOKENS * 4) break
    const item = { role: message.role, text: raw }
    const tokens = historyTokens(item)
    if (tokens > remaining) break
    remaining -= tokens
    recent.push(item)
  }
  return recent.reverse()
}

/** Live AppState retains assistant events, not submitted user messages. Read
 * the already-committed prose instead; never reload the session transcript.
 * rawMarkdown is shared by every rendered line, so take only the zone end. */
export function suggestionHistoryFromLines(lines: readonly OutputLine[]): UIMessage[] {
  const recent: UIMessage[] = []
  let remaining = HISTORY_TOKENS
  for (let i = lines.length - 1; i >= 0; i--) {
    const row = lines[i]!
    const role = row.kind === 'user' ? 'user' : row.kind === 'assistant' && row.rawMarkdown && row.zoneEnd
      && recent.at(-1)?.role !== 'assistant' ? 'assistant' : null
    if (!role) continue
    const text = role === 'user' ? row.text : row.rawMarkdown!
    if (!text.trim() || text.length > HISTORY_TOKENS * 4) break
    const tokens = historyTokens({ role, text })
    if (tokens > remaining) break
    remaining -= tokens
    recent.push({ id: row.id, role, text, timestamp: row.timestamp ?? 0 })
  }
  return recent.reverse()
}

/** Recognize authoritative answers; sampling starts only once the run is
 * idle. Never sample deltas, commentary or a tool-bearing response. */
export function isSuggestionBoundary(event: { kind: string; payload: Record<string, unknown> }): boolean {
  if (event.kind !== 'assistant_completed' || event.payload.stop_reason !== 'stop') return false
  const content = event.payload.content
  return Array.isArray(content)
    && content.some(block => block.type === 'text' && block.text?.trim())
    && !content.some(block => block.type === 'tool_call')
}

/** Request ownership is independent of Run ownership. No await on the input
 * or event path; cancellation also fences transports which resolve late. */
export class PromptSuggestion {
  private generation = 0
  private request: NativeSuggestion | null = null
  private value: { key: string; text: string } | null = null
  private lastAttempt = ''

  constructor(private readonly host: {
    key(): string
    eligible(): boolean
    allowsCommand?(text: string): boolean
    start(messages: SuggestionMessage[]): NativeSuggestion
    changed(): void
  }) {}

  cancel(): void {
    this.generation++
    this.request?.abort()
    this.request = null
    if (this.value) {
      this.value = null
      this.host.changed()
    }
  }

  start(messages: readonly UIMessage[], trigger: SuggestionTrigger = 'answer'): void {
    if (!this.host.eligible()) return
    const context = suggestionContext(messages)
    const lastUser = context.findLast(message => message.role === 'user')
    if (!lastUser || context.at(-1)?.role !== 'assistant' || context.filter(message => message.role === 'assistant').length < 2
      || (trigger === 'answer' && lastUser.text.trimStart().startsWith('/'))) return
    const key = this.host.key()
    const attempt = JSON.stringify([key, trigger, context])
    if (attempt === this.lastAttempt) return
    this.cancel()
    this.lastAttempt = attempt
    const generation = this.generation
    try {
      const request = this.host.start(context)
      this.request = request
      void request.result().then(text => {
        if (generation !== this.generation || key !== this.host.key()) return
        this.request = null
        if (!this.host.eligible() || !text?.trim()) return
        // Defense in depth for a mismatched addon: ghost text is one short
        // plain line. Slash actions must be exact registered actions offered
        // by the current product state, and still only fill the editor.
        if (text.length > 320 || /[\p{Cc}\p{Cf}]/u.test(text) || /^\s*[`<]/.test(text)) return
        const trimmed = text.trim()
        if (trimmed.startsWith('/') && !this.host.allowsCommand?.(trimmed)) return
        this.value = { key, text: trimmed }
        this.host.changed()
      }).catch(() => {
        if (generation === this.generation) this.request = null
      })
    } catch {
      // Missing/old addon, unavailable service, and failures are all silent.
    }
  }

  /** Reconcile host state transitions (including asynchronously opened
   * overlays or attachments) without starting any new request on redraw. */
  reconcile(): void {
    if ((this.request || this.value) && !this.host.eligible()) this.cancel()
  }

  text(): string {
    if (!this.host.eligible() || this.value?.key !== this.host.key()) return ''
    if (this.value.text.startsWith('/') && !this.host.allowsCommand?.(this.value.text)) return ''
    return this.value.text
  }

  take(): string {
    const text = this.text()
    this.cancel()
    return text
  }
}
