import type { Confidence, ConnectionInfo, HealthReport, Issue, Remediation } from "../types";
import type { ProviderConfig } from "../store/persist";

// Re-export the Health front-door wire types so components can import them
// alongside the fetch helper (mirrors how Recommendation lives here).
export type {
  HealthReport,
  Issue,
  IssueSeverity,
  FixAction,
  Metric,
  Confidence,
  SeverityCounts,
  SignalSummary,
  Remediation,
  RemediationStep,
  SolutionOption,
  RiskLevel,
} from "../types";

const BASE = "/api";

export async function backendHealthy(): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/health`, { method: "GET" });
    return r.ok;
  } catch {
    return false;
  }
}

export type Capabilities = { integrated_auth: boolean };

/** What this backend binary actually supports. Defaults to the safe assumption
 *  (no integrated auth) if the call fails, so the UI never offers a dead end. */
export async function capabilities(): Promise<Capabilities> {
  try {
    const r = await fetch(`${BASE}/capabilities`, { method: "GET" });
    if (!r.ok) return { integrated_auth: false };
    return await r.json();
  } catch {
    return { integrated_auth: false };
  }
}

export async function connect(info: ConnectionInfo) {
  const r = await fetch(`${BASE}/connect`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  return r.json() as Promise<{ ok: boolean; server_version: string | null; error: string | null }>;
}

export interface ScanObjectResult {
  schema_name: string;
  object_name: string;
  object_type: string;
  body_length: number;
  findings_total: number;
  findings_critical: number;
  findings_error: number;
  findings_warning: number;
  findings_info: number;
  top_rules: string[];
}

export interface ScanResult {
  server: string;
  database: string | null;
  objects_scanned: number;
  findings_total: number;
  findings_critical: number;
  findings_error: number;
  findings_warning: number;
  findings_info: number;
  rule_incidence: Array<[string, number]>;
  objects: ScanObjectResult[];
  duration_ms: number;
}

export async function scanDatabase(info: ConnectionInfo, server_version: number): Promise<ScanResult> {
  const r = await fetch(`${BASE}/scan/database`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...info, server_version }),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `scan failed (${r.status})`);
  }
  return r.json();
}

export async function listDatabases(info: ConnectionInfo): Promise<string[]> {
  const r = await fetch(`${BASE}/databases`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `database list failed (${r.status})`);
  }
  const body = (await r.json()) as { databases?: string[] };
  return body.databases ?? [];
}

export type RecommendationKind =
  | "create_index"
  | "drop_index"
  | "merge_index"
  | "columnstore_candidate";

export type RecommendationPriority = "high" | "medium" | "low";

export interface Recommendation {
  kind: RecommendationKind;
  priority: RecommendationPriority;
  title: string;
  object: string;        // schema.table[.index]
  rationale: string;
  ddl: string;           // exact T-SQL, multi-line
  impact_score: number;
  /**
   * Grounded evidence chips (label/value), drawn from the SAME DMV numbers the
   * rationale is built from. Serde-defaults to `[]` on the backend, so it is
   * optional here for older payloads.
   */
  metrics?: Array<[string, string]>;
  /**
   * Provenance of the numbers — "observed" | "estimated" | "heuristic"
   * (analyzer-core Recommendation.confidence; serde-defaults "observed").
   * Columnstore recs are "heuristic" (rule-of-thumb compression ratios).
   */
  confidence?: Confidence;
}

/**
 * Server-level prescriptive advisor. Mirrors pullDmv/listDatabases payload +
 * error handling. The backend ranks recommendations high→low before returning.
 */
export async function advise(info: ConnectionInfo): Promise<{ recommendations: Recommendation[] }> {
  const r = await fetch(`${BASE}/advise`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `advise failed (${r.status})`);
  }
  const body = (await r.json()) as { recommendations?: Recommendation[] };
  return { recommendations: body.recommendations ?? [] };
}

/**
 * Health front-door. Server-side aggregated, engine-neutral report fusing
 * advisor recs + static findings + sentinel pain into one ranked HealthReport.
 *
 * POSTs to /health/db (NOT GET /health — that is the liveness probe used by
 * backendHealthy()). Same payload + ok/error unwrap as advise(); the engine
 * param lets future Postgres/MySQL providers plug in behind the same endpoint.
 */
export async function getDbHealth(info: ConnectionInfo, engine = "sqlserver"): Promise<HealthReport> {
  const r = await fetch(`${BASE}/health/db`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...info, engine }),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `health scan failed (${r.status})`);
  }
  return r.json() as Promise<HealthReport>;
}

/**
 * Lazy issue-detail enrichment for the investigate kinds only
 * (deadlock/blocking/wait/regression — Issue.fix_action === "investigate").
 *
 * POSTs the Issue identity + connection to /api/health/issue/detail and gets
 * back ONE structured Remediation built from live sentinel data the backend
 * already holds (parsed deadlock graph, blocking sample, wait-type table,
 * regression row). The four advisor kinds + `finding` never call this — they
 * are templated client-side from fields already on the Issue.
 *
 * The backend returns a GRACEFUL Remediation (never 500) for parse misses, so
 * a non-ok status here is a genuine error (e.g. unknown kind → 400).
 */
export async function getIssueDetail(info: ConnectionInfo, issue: Issue): Promise<Remediation> {
  const r = await fetch(`${BASE}/health/issue/detail`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      ...info,
      issue_id: issue.id,
      issue_kind: issue.kind,
      affected_object: issue.affected_object,
    }),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `issue detail failed (${r.status})`);
  }
  return r.json() as Promise<Remediation>;
}

// ---- Live activity monitor (real-time server vitals) ---------------------
export type LiveWait = { wait_type: string; tasks: number; wait_ms: number };
export type LiveSession = {
  session_id: number; status: string; command: string; duration_ms: number;
  cpu_ms: number; logical_reads: number; blocked_by: number; wait_type: string | null;
  database: string; login: string; host: string; program: string; sql_preview: string;
};
export type LiveMetrics = {
  server_time_ms: number;
  cpu_sql_pct: number | null; cpu_other_pct: number | null;
  waiting_tasks: number; active_requests: number; blocked_requests: number; user_sessions: number;
  batch_requests_total: number; compilations_total: number; recompilations_total: number;
  transactions_total: number; page_life_expectancy: number | null;
  io_read_bytes_total: number; io_write_bytes_total: number;
  io_stall_read_ms: number; io_stall_write_ms: number;
  top_waits: LiveWait[]; sessions: LiveSession[];
};

/** One real-time snapshot of server vitals. The caller polls this on an
 *  interval and derives per-second rates from successive cumulative counters. */
export async function liveMetrics(info: ConnectionInfo): Promise<LiveMetrics> {
  const r = await fetch(`${BASE}/monitor/live`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `live monitor failed (${r.status})`);
  }
  return r.json() as Promise<LiveMetrics>;
}

export async function pullDmv(info: ConnectionInfo) {
  const r = await fetch(`${BASE}/dmv`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  if (!r.ok) throw new Error((await r.text()) || "DMV pull failed");
  return r.json();
}

export async function explain(info: ConnectionInfo, sql: string): Promise<string> {
  const r = await fetch(`${BASE}/explain`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...info, sql }),
  });
  const json = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error((json as any).error ?? `explain failed (${r.status})`);
  return (json as any).plan_xml as string;
}

export type ParseDiagnostic = { number: number; line: number; message: string };
export type ValidateResult = { ok: boolean; diagnostics: ParseDiagnostic[] };

/** SSMS-style "Parse" of a T-SQL batch against the real engine (SET PARSEONLY
 *  ON). Verifies syntax + keywords for the connected server's version without
 *  executing or binding object names. `ok:true` + empty diagnostics = clean. */
export async function validateSql(info: ConnectionInfo, sql: string): Promise<ValidateResult> {
  const r = await fetch(`${BASE}/validate`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...info, sql }),
  });
  const json = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error((json as any).error ?? `validate failed (${r.status})`);
  return json as ValidateResult;
}

export async function listOllamaModels(): Promise<{ models?: { name: string }[] } | null> {
  try {
    const r = await fetch(`${BASE}/llm/models`);
    if (!r.ok) return null;
    return r.json();
  } catch {
    return null;
  }
}

export interface ChatMessage { role: "system" | "user" | "assistant"; content: string }

/** Stream from Ollama via the backend SSE endpoint. */
export async function chatStream(
  model: string,
  messages: ChatMessage[],
  onToken: (s: string) => void,
  signal?: AbortSignal,
): Promise<void> {
  const r = await fetch(`${BASE}/llm/chat`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model, messages }),
    signal,
  });
  await consumeSse(r, onToken);
}

/** Stream from a cloud provider through the backend proxy. */
export async function cloudChatStream(
  p: ProviderConfig,
  messages: ChatMessage[],
  onToken: (s: string) => void,
  signal?: AbortSignal,
): Promise<void> {
  const r = await fetch(`${BASE}/llm/cloud/${p.key}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ config: p, messages }),
    signal,
  });
  await consumeSse(r, onToken);
}

export interface CloudModel {
  id: string;
  name?: string | null;
  context?: number | null;
  price_in?: number | null;   // USD per 1M prompt tokens
  price_out?: number | null;  // USD per 1M completion tokens
  free: boolean;
}

export interface KeyTestResult {
  ok: boolean;
  detail: string;
  credits_remaining?: number | null;
}

/** Providers that support backend key-test + model discovery. */
export const DISCOVERY_PROVIDERS = ["openai", "openrouter", "anthropic"] as const;

/** POST JSON with an abort-on-timeout guard so a hung call can't spin forever. */
async function postJson(path: string, body: unknown, timeoutMs = 25_000): Promise<any> {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), timeoutMs);
  let r: Response;
  try {
    r = await fetch(`${BASE}${path}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: ctrl.signal,
    });
  } catch (e: any) {
    throw new Error(e?.name === "AbortError" ? "timed out — is the backend reachable?" : (e?.message ?? "network error"));
  } finally {
    clearTimeout(timer);
  }
  const j = await r.json().catch(() => ({}));
  if (!r.ok || (j as any).error) throw new Error((j as any).error || `HTTP ${r.status}`);
  return j;
}

/** List a cloud provider's models (proxied through the backend). */
export async function listCloudModels(providerKey: string, config: unknown): Promise<CloudModel[]> {
  const j = await postJson(`/llm/cloud/${providerKey}/models`, { config });
  return j.models as CloudModel[];
}

/** Validate a cloud provider API key (and report credits for OpenRouter). */
export async function testCloudKey(providerKey: string, config: unknown): Promise<KeyTestResult> {
  return (await postJson(`/llm/cloud/${providerKey}/test`, { config })) as KeyTestResult;
}

async function consumeSse(r: Response, onToken: (s: string) => void) {
  if (!r.ok || !r.body) {
    let detail = "";
    try { detail = await r.text(); } catch {}
    throw new Error(`LLM ${r.status}${detail ? `: ${detail}` : ""}`);
  }
  const reader = r.body.getReader();
  const dec = new TextDecoder();
  let buf = "";
  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    let idx: number;
    while ((idx = buf.indexOf("\n\n")) !== -1) {
      const frame = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      let evt = "message";
      let data = "";
      for (const line of frame.split("\n")) {
        if (line.startsWith("event:")) evt = line.slice(6).trim();
        else if (line.startsWith("data:")) data += line.slice(5).replace(/^ /, "");
      }
      if (evt === "done") return;
      if (evt === "error") throw new Error(data || "stream error");
      if (data) onToken(data);
    }
  }
}
