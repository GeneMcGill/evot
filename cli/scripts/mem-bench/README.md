# Memory bench

Drives the real `dist/evot` binary in a PTY against a fake OpenAI-compatible
server that answers every call with one `bash` tool call producing a large
output, and samples the process with `ps`/`footprint` (macOS) every 5 s.

```sh
make build
cd cli
bun run scripts/mem-bench/mem-repro.ts                       # 300 turns, 200k window
CTX=1000000 FAKE_TURNS=400 bun run scripts/mem-bench/mem-repro.ts
EXPANDED=1 bun run scripts/mem-bench/mem-repro.ts            # ctrl+o view
KEEP_HOME=1 bun run scripts/mem-bench/mem-repro.ts           # keep logs for inspection
MallocStackLogging=malloc INSPECT_SECONDS=600 bun run scripts/mem-bench/mem-repro.ts
#   then: malloc_history <pid> -callTree -invert
```

Knobs: `FAKE_TURNS`, `FAKE_LINES` (output lines per call), `CTX` (context
window advertised to evot), `MAX_SECONDS`, `INSPECT_SECONDS` (keep the process
alive after the run for `footprint`/`vmmap`/`malloc_history`), `FAKE_PORT`.

The fake server treats a request without `tools` as a compaction summary
request and answers with a short summary, so compaction cycles run end to end.
