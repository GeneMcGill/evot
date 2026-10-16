import type { SuggestionProductContext } from '../../src/native/contracts/suggestion.js'

export function suggestionMessageText(content: unknown): string {
  if (typeof content === 'string') return content
  return Array.isArray(content) ? content.map(block => typeof block.text === 'string' ? block.text : '').join('') : ''
}

/** Extract only the metadata fragment; the conversation must remain native
 * user/assistant messages, not an encoded conversation object. */
export function suggestionRequestContext(request: Record<string, unknown>): {
  original_task: string
  language_reference: string
  product_context?: SuggestionProductContext
} {
  const messages = request.messages as { role: string; content: string | { type: string; text?: string }[] }[]
  const text = suggestionMessageText(messages.at(-1)?.content)
  const match = /<suggestion_context>\n([\s\S]*?)\n<\/suggestion_context>/.exec(text)
  if (!match) throw new Error('Missing final suggestion context fragment')
  return JSON.parse(match[1]!)
}
