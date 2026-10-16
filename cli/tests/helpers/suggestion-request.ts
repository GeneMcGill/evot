export function suggestionMessageText(content: unknown): string {
  if (typeof content === 'string') return content
  return Array.isArray(content) ? content.map(block => typeof block.text === 'string' ? block.text : '').join('') : ''
}

/** The model input ends with only the Codex instruction, not a UI/goal JSON
 * fragment. History before it retains native user/assistant roles. */
export function suggestionInstructions(request: Record<string, unknown>): string {
  const messages = request.messages as { role: string; content: unknown }[]
  const last = messages.at(-1)
  const text = suggestionMessageText(last?.content)
  if (last?.role !== 'user' || !text.startsWith('<next_prompt_suggestion>\n') || !text.endsWith('\n</next_prompt_suggestion>')) {
    throw new Error('Missing final Codex suggestion instruction')
  }
  if (['suggestion_context', 'language_reference', 'original_task', 'product_context'].some(field => text.includes(field))) {
    throw new Error('Unexpected extra prompt metadata')
  }
  return text
}
