/**
 * Process memory snapshot for the TUI.
 *
 * A memory report from the field ("evot is at 8 GB") is only actionable with
 * numbers from inside the process: resident size, JS heap, and the sizes of
 * the structures this client retains for the whole session. One line per run
 * in the screen log gives the trend; `/_mem` prints the same line on demand.
 */

export interface MemoryRetained {
  /** Rows kept for the compact (default) view. */
  compactRows: number
  /** Rows kept for the expanded (ctrl+o) view. */
  expandedRows: number
  /** Completed assistant messages kept in app state. */
  messages: number
}

export interface MemorySnapshot {
  rssBytes: number
  heapUsedBytes: number
  heapTotalBytes: number
  externalBytes: number
  retained: MemoryRetained
}

export function memorySnapshot(retained: MemoryRetained): MemorySnapshot {
  const usage = process.memoryUsage()
  return {
    rssBytes: usage.rss,
    heapUsedBytes: usage.heapUsed,
    heapTotalBytes: usage.heapTotal,
    externalBytes: usage.external,
    retained,
  }
}

/** `[MEM] rss 812M · js heap 231M/300M · external 12M · rows 18k/64k · msgs 32` */
export function formatMemoryLine(snapshot: MemorySnapshot): string {
  const { retained } = snapshot
  return '[MEM] ' + [
    `rss ${formatBytes(snapshot.rssBytes)}`,
    `js heap ${formatBytes(snapshot.heapUsedBytes)}/${formatBytes(snapshot.heapTotalBytes)}`,
    `external ${formatBytes(snapshot.externalBytes)}`,
    `rows ${formatCount(retained.compactRows)}/${formatCount(retained.expandedRows)}`,
    `msgs ${retained.messages}`,
  ].join(' · ')
}

export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)}G`
  if (bytes >= 1024 * 1024) return `${Math.round(bytes / (1024 * 1024))}M`
  if (bytes >= 1024) return `${Math.round(bytes / 1024)}K`
  return `${bytes}B`
}

function formatCount(count: number): string {
  return count >= 1000 ? `${(count / 1000).toFixed(count >= 10_000 ? 0 : 1)}k` : String(count)
}
