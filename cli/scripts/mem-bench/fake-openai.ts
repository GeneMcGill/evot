// Fake OpenAI chat-completions server for the memory bench.
//
// Every call with tools returns one `bash` tool call that prints LINES lines,
// for TURNS turns, then a short final text. A call without tools is a
// compaction summary request and gets a short summary. Usage is reported the
// way a real provider would: proportional to the request's message bytes.
const PORT = Number(process.env.FAKE_PORT ?? 8787)
const TURNS = Number(process.env.FAKE_TURNS ?? 300)
const LINES = Number(process.env.FAKE_LINES ?? 2000)
let calls = 0

function sse(chunks: unknown[]): Response {
  const body = chunks.map(c => `data: ${JSON.stringify(c)}\n\n`).join('') + 'data: [DONE]\n\n'
  return new Response(body, { headers: { 'content-type': 'text/event-stream' } })
}

Bun.serve({
  port: PORT,
  async fetch(req) {
    const url = new URL(req.url)
    if (!url.pathname.endsWith('/chat/completions')) return new Response('nf', { status: 404 })
    const body = await req.json() as { messages: unknown[]; tools?: unknown[] }
    if (!body.tools || body.tools.length === 0) {
      const sid = `sum-${Date.now()}`
      const sbase = { id: sid, object: 'chat.completion.chunk', created: 0, model: 'fake' }
      console.log(`summary request (${body.messages.length} msgs)`)
      return sse([
        { ...sbase, choices: [{ index: 0, delta: { role: 'assistant', content: '## Summary\nRan many bash steps producing log output.' }, finish_reason: null }] },
        { ...sbase, choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 } },
      ])
    }
    calls++
    const id = `chatcmpl-${calls}`
    const base = { id, object: 'chat.completion.chunk', created: 0, model: 'fake' }
    const promptTokens = Math.floor(JSON.stringify(body.messages).length / 4)
    const usage = { prompt_tokens: promptTokens, completion_tokens: 30, total_tokens: promptTokens + 30 }
    if (calls > TURNS) {
      return sse([
        { ...base, choices: [{ index: 0, delta: { role: 'assistant', content: 'done.' }, finish_reason: null }] },
        { ...base, choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage },
      ])
    }
    const args = JSON.stringify({
      command: `for i in $(seq 1 ${LINES}); do echo "line $i: some fairly ordinary log output text here padded to be realistic"; done`,
      description: `turn ${calls}`,
    })
    return sse([
      { ...base, choices: [{ index: 0, delta: { role: 'assistant', content: `Step ${calls}: running.` }, finish_reason: null }] },
      { ...base, choices: [{ index: 0, delta: { tool_calls: [{ index: 0, id: `call_${calls}`, type: 'function', function: { name: 'bash', arguments: args } }] }, finish_reason: null }] },
      { ...base, choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }], usage },
    ])
  },
})
console.log(`fake openai on ${PORT} turns=${TURNS} lines=${LINES}`)
