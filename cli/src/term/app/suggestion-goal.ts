import { COMPACT_SUMMARY_PREFIX } from './resume.js'
import type { UIMessage } from './types.js'

/** Kept outside the rendered/recent history. Compaction and scrollback limits
 * must not silently change the initial request; explicit clear/new does. */
export class SuggestionGoal {
  private task: string | undefined

  observe(text: string): void {
    if (this.task || !text.trim() || text.trimStart().startsWith(COMPACT_SUMMARY_PREFIX)) return
    const chars = Array.from(text.trim())
    this.task = chars.length <= 512 ? chars.join('') : `${chars.slice(0, 170).join('')}…${chars.slice(-341).join('')}`
  }

  restore(messages: readonly UIMessage[]): void {
    this.reset()
    for (const message of messages) {
      if (message.role === 'user') this.observe(message.text)
      if (this.task) break
    }
  }

  reset(): void { this.task = undefined }
  value(): string | undefined { return this.task }
}
