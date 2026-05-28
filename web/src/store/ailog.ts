/**
 * LLM interaction log.
 *
 * In-memory ring buffer for instant UI rendering, mirrored write-through to
 * the backend's `/api/logs/ai` endpoint so entries survive browser cache
 * clears AND backend restarts (SQLite at `~/.sqlopt/sentinel.db`).
 *
 * Streaming responses post twice: once on `startEntry` (placeholder) and once
 * on `finishEntry` (final state). `appendToken` is local-only to avoid
 * hammering the backend with one POST per token.
 *
 * Capped at MAX entries in memory. Backend keeps the full history.
 */

export type AiLogStatus = "streaming" | "ok" | "error" | "cancelled";

export interface AiLogEntry {
  id: string;
  timestamp: number;
  provider: string;
  model: string;
  system?: string;
  user: string;
  response: string;
  latency_ms: number | null;
  tokens_in?: number;
  tokens_out?: number;
  status: AiLogStatus;
  error?: string;
}

const MAX = 500;
const SUBS = new Set<() => void>();
let LOG: AiLogEntry[] = [];

function notify() { for (const fn of SUBS) fn(); }

export function getAll(): AiLogEntry[] { return LOG; }

export function subscribe(fn: () => void): () => void {
  SUBS.add(fn);
  return () => SUBS.delete(fn);
}

// Fire-and-forget write-through to the durable store. Network failures are
// silent: the UI ring buffer is still authoritative for the current session.
function syncEntry(e: AiLogEntry): void {
  void fetch("/api/logs/ai", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      id: e.id,
      provider: e.provider,
      model: e.model,
      system_prompt: e.system ?? null,
      user_prompt: e.user,
      response: e.response,
      status: e.status,
      error_message: e.error ?? null,
      latency_ms: e.latency_ms,
      tokens_in: e.tokens_in ?? null,
      tokens_out: e.tokens_out ?? null,
      occurred_at: new Date(e.timestamp).toISOString(),
    }),
  }).catch(() => {});
}

export function startEntry(seed: Omit<AiLogEntry, "id" | "timestamp" | "response" | "latency_ms" | "status">): string {
  const id = crypto.randomUUID();
  const entry: AiLogEntry = {
    id,
    timestamp: Date.now(),
    response: "",
    latency_ms: null,
    status: "streaming",
    ...seed,
  };
  LOG = [entry, ...LOG].slice(0, MAX);
  notify();
  syncEntry(entry);
  return id;
}

export function appendToken(id: string, tok: string) {
  const ix = LOG.findIndex((e) => e.id === id);
  if (ix < 0) return;
  LOG[ix] = { ...LOG[ix], response: LOG[ix].response + tok };
  notify();
  // intentionally not synced per token — flushed in finishEntry
}

export function finishEntry(id: string, status: AiLogStatus, error?: string) {
  const ix = LOG.findIndex((e) => e.id === id);
  if (ix < 0) return;
  const e = LOG[ix];
  LOG[ix] = {
    ...e,
    status,
    error,
    latency_ms: Date.now() - e.timestamp,
    tokens_out: e.response.length, // char-count fallback; LLM token counts vary by tokenizer
    tokens_in: (e.system?.length ?? 0) + e.user.length,
  };
  notify();
  syncEntry(LOG[ix]);
}

export function clear() {
  LOG = [];
  notify();
  void fetch("/api/logs/ai", { method: "DELETE" }).catch(() => {});
}

// Backfill the ring buffer from the durable store. Call once on app boot.
export async function hydrate(): Promise<void> {
  try {
    const r = await fetch("/api/logs/ai?limit=500");
    if (!r.ok) return;
    const body = (await r.json()) as { entries: Array<{
      id: string;
      occurred_at: string;
      provider: string;
      model: string;
      system_prompt: string | null;
      user_prompt: string;
      response: string;
      status: AiLogStatus;
      error_message: string | null;
      latency_ms: number | null;
      tokens_in: number | null;
      tokens_out: number | null;
    }> };
    LOG = body.entries.map((e) => ({
      id: e.id,
      timestamp: Date.parse(e.occurred_at) || Date.now(),
      provider: e.provider,
      model: e.model,
      system: e.system_prompt ?? undefined,
      user: e.user_prompt,
      response: e.response,
      latency_ms: e.latency_ms,
      tokens_in: e.tokens_in ?? undefined,
      tokens_out: e.tokens_out ?? undefined,
      status: e.status,
      error: e.error_message ?? undefined,
    }));
    notify();
  } catch {
    // backend offline — ring buffer stays empty, next session writes will sync
  }
}

function redactKeys(s: string): string {
  if (!s) return s;
  // Best-effort: redact common key patterns
  return s
    .replace(/sk-[A-Za-z0-9_\-]{20,}/g, "sk-•••••REDACTED•••••")
    .replace(/(api[_-]?key["':\s=]+["']?)[A-Za-z0-9_\-]{20,}/gi, "$1•••••REDACTED•••••")
    .replace(/(AKIA[0-9A-Z]{16})/g, "•••AWS_KEY_REDACTED•••")
    .replace(/(?<=Bearer\s)[A-Za-z0-9_\-.]+/g, "•••••REDACTED•••••");
}

export function exportJson(redact = true): string {
  const cleaned = LOG.map((e) => ({
    ...e,
    system: redact ? redactKeys(e.system ?? "") : (e.system ?? ""),
    user: redact ? redactKeys(e.user) : e.user,
    response: redact ? redactKeys(e.response) : e.response,
    error: redact && e.error ? redactKeys(e.error) : e.error,
  }));
  return JSON.stringify({ exported_at: new Date().toISOString(), entries: cleaned }, null, 2);
}

export function exportCsv(redact = true): string {
  const header = ["timestamp", "provider", "model", "status", "latency_ms", "tokens_in", "tokens_out", "user_chars", "response_chars", "error"];
  const rows = LOG.map((e) => {
    const fields = [
      new Date(e.timestamp).toISOString(),
      e.provider,
      e.model,
      e.status,
      e.latency_ms ?? "",
      e.tokens_in ?? "",
      e.tokens_out ?? "",
      e.user.length,
      e.response.length,
      e.error ? (redact ? redactKeys(e.error) : e.error) : "",
    ];
    return fields.map(csvEsc).join(",");
  });
  return [header.join(","), ...rows].join("\n");
}

function csvEsc(v: any): string {
  const s = String(v ?? "");
  if (s.includes(",") || s.includes('"') || s.includes("\n")) {
    return `"${s.replace(/"/g, '""')}"`;
  }
  return s;
}

export function download(filename: string, mime: string, body: string) {
  const blob = new Blob([body], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}
