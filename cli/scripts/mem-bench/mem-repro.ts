// Drive the real evot binary in a PTY against the fake OpenAI server and
// sample RSS over a long tool-heavy session.
import { spawn } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const EVOT_BIN = process.env.EVOT_TEST_BIN || join(import.meta.dirname, '..', '..', 'dist', 'evot')
const PORT = Number(process.env.FAKE_PORT ?? 8787)
const TURNS = Number(process.env.FAKE_TURNS ?? 300)
const LINES = Number(process.env.FAKE_LINES ?? 2000)
const EXPANDED = process.env.EXPANDED === '1'

const home = mkdtempSync(join(tmpdir(), 'evot-mem-'))
const state = join(home, '.evotai')
mkdirSync(state, { recursive: true })
writeFileSync(join(state, 'evot.env'), [
  'EVOT_LLM_PROVIDER=fake',
  'EVOT_LLM_FAKE_API_KEY=fake-key',
  `EVOT_LLM_FAKE_BASE_URL=http://127.0.0.1:${PORT}/v1`,
  'EVOT_LLM_FAKE_MODEL=fake',
  'EVOT_LLM_FAKE_PROTOCOL=openai',
  `EVOT_LLM_FAKE_CONTEXT_WINDOW=${process.env.CTX ?? 200000}`,
  '',
].join('\n'))
const work = join(home, 'work')
mkdirSync(work)

const server = spawn('bun', ['run', join(import.meta.dirname, 'fake-openai.ts')], {
  env: { ...process.env, FAKE_PORT: String(PORT), FAKE_TURNS: String(TURNS), FAKE_LINES: String(LINES) },
  stdio: ['ignore', 'inherit', 'inherit'],
})
await Bun.sleep(500)

const PTY = `
import os, pty, select, sys, signal
pid, fd = pty.fork()
if pid == 0:
    os.environ['TERM'] = 'xterm-256color'
    os.environ['COLUMNS'] = '160'
    os.environ['LINES'] = '50'
    os.execv(sys.argv[1], sys.argv[1:])
print("CHILD_PID", pid, flush=True)
try:
    while True:
        r, _, _ = select.select([sys.stdin.buffer, fd], [], [], 0.1)
        if sys.stdin.buffer in r:
            data = sys.stdin.buffer.read1(4096)
            if not data: break
            os.write(fd, data)
        if fd in r:
            try: data = os.read(fd, 65536)
            except OSError: break
            if not data: break
            sys.stdout.buffer.write(data); sys.stdout.buffer.flush()
    os.waitpid(pid, 0)
except KeyboardInterrupt:
    pass
finally:
    try: os.kill(pid, signal.SIGTERM)
    except ProcessLookupError: pass
`
const env = {
  PATH: process.env.PATH, LANG: 'en_US.UTF-8', TMPDIR: process.env.TMPDIR,
  HOME: home, USERPROFILE: home, TERM: 'xterm-256color', COLORTERM: 'truecolor',
  EVOT_THEME: 'dark', EVOT_MOUSE: '0', EVOT_AUTO_DOWNLOAD: '0',
  EVOT_HOME: state, EVOT_STORAGE_FS_ROOT_DIR: state,
  ...(process.env.MallocStackLogging ? { MallocStackLogging: process.env.MallocStackLogging } : {}),
}
const child = spawn('python3', ['-c', PTY, EVOT_BIN], { cwd: work, env, stdio: ['pipe', 'pipe', 'inherit'] })
let out = ''
let childPid = 0
child.stdout.on('data', (d: Buffer) => {
  const s = d.toString()
  const m = s.match(/CHILD_PID (\d+)/)
  if (m) childPid = Number(m[1])
  out += s
  if (out.length > 200_000) out = out.slice(-100_000)
})

function rss(pid: number): number {
  const r = Bun.spawnSync(['ps', '-o', 'rss=', '-p', String(pid)])
  return Number(r.stdout.toString().trim()) / 1024
}
function count(re: RegExp): number { return (out.match(re) ?? []).length }

await Bun.sleep(4000)
if (EXPANDED) { child.stdin.write('\x0f'); await Bun.sleep(300) } // ctrl+o
child.stdin.write('go\r')
const start = Date.now()
const peak = { rss: 0 }
const timer = setInterval(() => {
  if (!childPid) return
  const mb = rss(childPid)
  peak.rss = Math.max(peak.rss, mb)
  const turns = count(/Step \d+: running/g)
  const fp = Bun.spawnSync(['footprint', String(childPid)]).stdout.toString()
  const pick = (name: string) => (fp.match(new RegExp(`^\\s*([\\d.]+ [KMG]B)\\s+\\S+ \\S*\\s+\\S+ \\S*\\s+\\d+\\s+${name}`, 'm')) ?? [])[1] ?? '?'
  const total = (fp.match(/Footprint: ([\d.]+ [KMG]B)/) ?? [])[1] ?? '?'
  process.stdout.write('')
  console.log(`[${((Date.now() - start) / 1000).toFixed(0)}s] rss=${mb.toFixed(0)}MB footprint=${total} js=${pick('WebKit Malloc')} small=${pick('Malloc Small')} large=${pick('Malloc Large')}`)
}, 5000)

const deadline = Date.now() + Number(process.env.MAX_SECONDS ?? 900) * 1000
while (Date.now() < deadline) {
  await Bun.sleep(1000)
  if (/done\./.test(out.slice(-4000)) && /Step/.test(out)) { await Bun.sleep(5000); break }
}
clearInterval(timer)
if (process.env.DUMP_PTY) await Bun.write(process.env.DUMP_PTY, out)
console.log(`FINAL rss=${rss(childPid).toFixed(0)}MB peak=${peak.rss.toFixed(0)}MB pid=${childPid}`)
await Bun.sleep(Number(process.env.INSPECT_SECONDS ?? 0) * 1000)
child.stdin.write('\x03'); await Bun.sleep(300); child.stdin.write('\x03'); await Bun.sleep(300)
child.stdin.write('/exit\r'); await Bun.sleep(1500)
child.kill('SIGTERM'); server.kill('SIGTERM')
if (process.env.KEEP_HOME) console.log(`HOME kept at ${home}`)
else rmSync(home, { recursive: true, force: true })
process.exit(0)
