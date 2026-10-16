import { expect, test } from 'bun:test'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { seedSmokeHome, smokeEnvironment } from './helpers/smoke-home.js'
import { suggestionRequestContext, suggestionMessageText } from './helpers/suggestion-request.js'

// Explicit opt-in to a freshly built addon; never use the developer's HOME.
const integration = process.env.EVOT_TEST_SUGGESTION_ADDON ? test : test.skip
for (const protocol of ['openai', 'anthropic'] as const) {
integration(`native ${protocol} suggestion preserves message roles and stops after withdrawal`, async () => {
  const home = mkdtempSync(join(tmpdir(), 'evot-suggestion-native-'))
  const requests: Record<string, unknown>[] = []
  const server = Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: async request => {
    expect(new URL(request.url).pathname).toBe(protocol === 'openai' ? '/v1/chat/completions' : '/v1/messages')
    const body = await request.json() as Record<string, unknown>
    requests.push(body)
    const input = suggestionRequestContext(body)
    const text = input.product_context ? '/done' : 'Run the tests'
    if (protocol === 'anthropic') {
      const events = [
        { type: 'message_start', message: { id: 'fixture', role: 'assistant', content: [], model: 'service-model', usage: { input_tokens: 10, output_tokens: 0 } } },
        { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
        { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } },
        { type: 'content_block_stop', index: 0 },
        { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 5 } },
        { type: 'message_stop' },
      ]
      return new Response(events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(''), { headers: { 'content-type': 'text/event-stream' } })
    }
    return new Response([
      `data: ${JSON.stringify({ id: 'fixture', model: 'service-model', choices: [{ index: 0, delta: { role: 'assistant', content: text }, finish_reason: null }] })}`,
      '',
      'data: {"id":"fixture","model":"service-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
      '', 'data: [DONE]', '', '',
    ].join('\n'), { headers: { 'content-type': 'text/event-stream' } })
  } })
  let child: ReturnType<typeof Bun.spawn> | undefined
  let deadline: ReturnType<typeof setTimeout> | undefined
  try {
    const root = seedSmokeHome(home)
    await Bun.write(join(root, 'auth.json'), JSON.stringify({
      version: 1, server_base_url: 'http://127.0.0.1:1',
      user: { id: 'fixture', name: 'Fixture', email: 'fixture@example.invalid' },
      cli_token: 'fixture-token', refresh_token: 'fixture-refresh', models_synced_at: 0,
    }))
    const endpoint = { model: 'service-model', protocol,
      base_url: `http://127.0.0.1:${server.port}${protocol === 'openai' ? '/v1' : ''}`, api_key: 'service-test-key', context_window: 8192 }
    await Bun.write(join(root, 'models.cache.json'), JSON.stringify({ schema_version: 1,
      synced_at: 1, response: { providers: [], models: [], services: { suggestion: endpoint } } }))
    child = Bun.spawn([process.execPath, '--eval', `
      const { NapiAgent } = require(${JSON.stringify(process.env.EVOT_TEST_SUGGESTION_ADDON)});
      const agent = await NapiAgent.create(null, null);
      const context = JSON.stringify([{role:'user',text:'Add the tests'}, {role:'assistant',text:'Tests are ready.'}]);
      const request = agent.suggestNextPrompt(context);
      if (await request.result() !== 'Run the tests') throw new Error('missing native suggestion');
      const product = { trigger: 'mode_changed', mode: 'plan', state: 'idle', input_behavior: 'Plan without edits.', original_task: 'Implement login with audit events', actions: [{text:'/done',description:'Return to normal mode.'}] };
      if (await agent.suggestNextPrompt(context, JSON.stringify(product)).result() !== '/done') throw new Error('missing product context action');
      const fs = await import('node:fs/promises');
      await fs.writeFile(${JSON.stringify(join(root, 'models.cache.json'))}, JSON.stringify({schema_version:1,synced_at:2,response:{services:{}}}));
      if (await agent.suggestNextPrompt(context).result() !== null) throw new Error('unexpected fallback');
      const cancelled = agent.suggestNextPrompt(context);
      cancelled.abort();
      if (await cancelled.result() !== null) throw new Error('cancelled request returned text');
      console.log('suggestion contract passed');
    `], { cwd: join(import.meta.dir, '..'), env: smokeEnvironment(home), stdout: 'pipe', stderr: 'pipe' })
    const handle = child
    deadline = setTimeout(() => handle.kill(), 10_000)
    const [code, stdout, stderr] = await Promise.all([
      child.exited, new Response(child.stdout).text(), new Response(child.stderr).text(),
    ])
    expect(code, stderr).toBe(0)
    expect(stdout.trim()).toBe('suggestion contract passed')
    expect(stdout + stderr).not.toContain('service-test-key')
    expect(requests).toHaveLength(2)
    expect(suggestionRequestContext(requests[1]!).product_context?.mode).toBe('plan')
    expect(suggestionRequestContext(requests[1]!).original_task).toBe('Implement login with audit events')
    const messages = requests[0]!.messages as { role: string; content: unknown }[]
    expect(messages.map(message => message.role)).toEqual(['user', 'assistant', 'user'])
    expect(suggestionMessageText(messages[0]!.content)).toBe('Add the tests')
    expect(suggestionMessageText(messages[1]!.content)).toBe('Tests are ready.')
    expect(suggestionMessageText(messages[2]!.content)).toContain('<next_prompt_suggestion>')
    expect(suggestionRequestContext(requests[0]!).original_task).toBe('Add the tests')
    expect(requests[0]!.model).toBe('service-model')
    expect(requests[0]!.max_tokens).toBe(32)
    expect(requests[0]!.tools ?? []).toEqual([])
    expect(JSON.stringify(requests[0]!.messages)).toContain('language_reference')
  } finally {
    if (deadline) clearTimeout(deadline)
    if (child) { child.kill(); await child.exited }
    await server.stop(true)
    rmSync(home, { recursive: true, force: true })
  }
}, 15_000)
}
