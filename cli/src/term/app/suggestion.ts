import type { NativeSuggestion } from '../../native/contracts/ports.js'
import type { UIMessage } from './types.js'
import type { OutputLine } from '../../render/output.js'
import type { SuggestionProductContext, SuggestionTrigger } from './composer-guidance.js'

export interface SuggestionMessage { role: 'user' | 'assistant'; text: string }

/** Only visible prose, never thinking, tool arguments/results, system notices
 * or the whole transcript. Rust independently applies its token/window cap. */
export function suggestionContext(messages: readonly UIMessage[]): SuggestionMessage[] {
  const recent: SuggestionMessage[] = []
  for (let i = messages.length - 1; i >= 0 && recent.length < 6; i--) {
    const message = messages[i]!
    if (message.role !== 'user' && message.role !== 'assistant') continue
    if (message.content?.some(block => block.type === 'tool_call')) continue
    const raw = message.role === 'assistant' && message.content
      ? message.content.filter(block => block.type === 'text').map(block => block.text).join('\n')
      : message.text
    if (!raw.trim()) continue
    const text = raw.length <= 1024 ? raw : `${raw.slice(0, 256)}…${raw.slice(-767)}`
    recent.push({ role: message.role, text })
  }
  return recent.reverse()
}

/** Live AppState retains assistant events, not submitted user messages. Read
 * the already-committed prose instead; never reload the session transcript.
 * rawMarkdown is shared by every rendered line, so take only the zone end. */
export function suggestionHistoryFromLines(lines: readonly OutputLine[]): UIMessage[] {
  const recent: UIMessage[] = []
  for (let i = lines.length - 1; i >= 0 && recent.length < 6; i--) {
    const row = lines[i]!
    if (row.kind === 'user') {
      recent.push({ id: row.id, role: 'user', text: row.text, timestamp: row.timestamp ?? 0 })
    } else if (row.kind === 'assistant' && row.rawMarkdown && row.zoneEnd
        && recent.at(-1)?.role !== 'assistant') {
      recent.push({ id: row.id, role: 'assistant', text: row.rawMarkdown, timestamp: row.timestamp ?? 0 })
    }
  }
  return recent.reverse()
}

/** Start at the earliest authoritative final-answer boundary, before run
 * teardown/cloud sync. Never sample every delta or a tool-bearing response. */
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
  private trigger: SuggestionTrigger = 'answer'

  constructor(private readonly host: {
    key(): string
    eligible(): boolean
    canSample?(trigger: SuggestionTrigger): boolean
    productContext?(trigger: SuggestionTrigger): SuggestionProductContext
    allowsCommand?(text: string): boolean
    start(messages: SuggestionMessage[], context?: SuggestionProductContext): NativeSuggestion
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
    if (!(this.host.canSample?.(trigger) ?? this.host.eligible())) return
    const context = suggestionContext(messages)
    const lastUser = context.findLast(message => message.role === 'user')
    if (!lastUser || (trigger === 'answer' && (context.at(-1)?.role !== 'assistant' || lastUser.text.trimStart().startsWith('/')))) return
    const key = this.host.key()
    const product = this.host.productContext?.(trigger)
    const attempt = JSON.stringify([key, trigger, context, product])
    if (attempt === this.lastAttempt) return
    this.cancel()
    this.lastAttempt = attempt
    this.trigger = trigger
    const generation = this.generation
    try {
      const request = this.host.start(context, product)
      this.request = request
      void request.result().then(text => {
        if (generation !== this.generation || key !== this.host.key()) return
        this.request = null
        if (!(this.host.canSample?.(trigger) ?? this.host.eligible()) || !text?.trim()) return
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
    if ((this.request || this.value) && !(this.host.canSample?.(this.trigger) ?? this.host.eligible())) this.cancel()
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
