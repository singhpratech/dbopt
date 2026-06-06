import type { Confidence, ConnectionInfo, HealthReport, Issue, Remediation } from "../types";
import type { ProviderConfig } from "../store/persist";

// Re-export the Health front-door wire types so components can import them
// alongside the fetch helper (mirrors how Recommendation lives here).
export type {
  HealthReport,
  BaselineTrend,
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

export type Capabilities = {
  /** Can do Windows integrated (current-user / trusted) auth on this build. */
  integrated_auth: boolean;
  /** Can authenticate with an explicit Windows account + password (NTLM). */
  windows_account_auth: boolean;
  /** AWS Bedrock provider is compiled into this build (opt-in feature). */
  bedrock: boolean;
  platform?: string;
  version?: string;
};

const CAPS_FALLBACK: Capabilities = { integrated_auth: false, windows_account_auth: false, bedrock: false };

/** What this backend binary actually supports. Defaults to the safe assumption
 *  (no Windows auth) if the call fails, so the UI never offers a dead end. */
export async function capabilities(): Promise<Capabilities> {
  try {
    const r = await fetch(`${BASE}/capabilities`, { method: "GET" });
    if (!r.ok) return CAPS_FALLBACK;
    return { ...CAPS_FALLBACK, ...(await r.json()) };
  } catch {
    return CAPS_FALLBACK;
  }
}

export type VersionInfo = { version: string; platform: string; arch: string };

/** The running binary's version + platform/arch (purely local — reads compile-
 *  time constants, makes no network call). Used by the in-app update check. */
export async function appVersion(): Promise<VersionInfo | null> {
  try {
    const r = await fetch(`${BASE}/version`, { method: "GET" });
    if (!r.ok) return null;
    return (await r.json()) as VersionInfo;
  } catch {
    return null;
  }
}

/** Ask the local backend to stop itself — the "Quit dbopt" step of the update
 *  flow, so an installer isn't blocked by the running binary. The server exits
 *  ~350ms after replying, so this resolves true on a clean 200 (and the backend
 *  goes away moments later). Returns false if the request can't be made. */
export async function shutdownBackend(): Promise<boolean> {
  try {
    const r = await fetch(`${BASE}/shutdown`, { method: "POST" });
    return r.ok;
  } catch {
    return false;
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

/** One database on the connected server, with grouping/gating metadata. */
export interface DatabaseInfo {
  name: string;
  /** master/tempdb/model/msdb (database_id <= 4). */
  system: boolean;
  /** ONLINE, RESTORING, RECOVERING, SUSPECT, OFFLINE, EMERGENCY, … */
  state: string;
  /** The connected login can actually open this database. */
  accessible: boolean;
}

export async function listDatabases(info: ConnectionInfo): Promise<DatabaseInfo[]> {
  const r = await fetch(`${BASE}/databases`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `database list failed (${r.status})`);
  }
  const body = (await r.json()) as { databases?: (string | DatabaseInfo)[] };
  // Tolerate an older backend that still returns a bare string[] (e.g. a stale
  // dev process on the port): treat each as an accessible, online user DB.
  return (body.databases ?? []).map((d) =>
    typeof d === "string"
      ? { name: d, system: false, state: "ONLINE", accessible: true }
      : d,
  );
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

// ---- Deep vitals (persisted scheduler/memory/IO/tempdb/plan-cache) -------
// Read-back of the background monitor's deepest telemetry. Field names mirror
// the persisted row structs (snake_case). Any surface can be null when the
// monitor hasn't captured it yet; io_latency is [] in that case.
export type CpuPressure = {
  captured_at: string;
  online_schedulers: number; runnable_tasks: number; work_queue: number;
  current_workers: number; active_workers: number; pending_disk_io: number;
};
export type MemoryHeadroom = {
  captured_at: string;
  page_life_expectancy: number; pending_memory_grants: number; granted_memory_kb: number;
  target_server_memory_kb: number; total_server_memory_kb: number;
};
export type IoLatencyFile = {
  captured_at: string;
  database_name: string; file_logical_name: string; file_type: string;
  reads_delta: number; writes_delta: number; read_stall_ms_delta: number; write_stall_ms_delta: number;
  avg_read_latency_ms: number; avg_write_latency_ms: number;
};
export type TempdbContention = {
  captured_at: string;
  pagelatch_waiters: number; pfs_waiters: number; gam_waiters: number; sgam_waiters: number;
  total_wait_ms: number; tempdb_data_files: number;
};
export type PlanCacheHealth = {
  captured_at: string;
  single_use_plan_count: number; single_use_size_kb: number;
  total_plan_count: number; total_size_kb: number;
};
/** One recent-trend point: [captured_at_ms, value]. Oldest→newest (freshest
 *  last) so a sparkline's last point is the most recent reading. */
export type VitalPoint = [number, number];

/** Recent time-series behind each headline deep-vital, for inline sparklines.
 *  Each is capped to the last ~60 captures and is `[]` until the monitor has
 *  recorded enough samples for this server (honest empty state). */
export type VitalSeries = {
  /** CPU LOAD — runnable tasks queued for a scheduler. */
  cpu_runnable_tasks: VitalPoint[];
  /** MEMORY HEADROOM — page-life-expectancy (seconds). */
  memory_ple: VitalPoint[];
  /** PLAN CACHE — single-use ad-hoc plan count. */
  plan_cache_single_use: VitalPoint[];
  /** CONTENTION (tempdb) — allocation-page waiters per tick. */
  tempdb_total_waiters: VitalPoint[];
  /** I/O LATENCY — worst avg read/write latency across files per tick (ms). */
  io_worst_latency_ms: VitalPoint[];
};

export type DeepVitals = {
  has_data: boolean;
  /** Newest captured_at across surfaces (epoch ms), or null when empty. */
  captured_at: number | null;
  cpu_pressure: CpuPressure | null;
  memory_headroom: MemoryHeadroom | null;
  io_latency: IoLatencyFile[];
  tempdb_contention: TempdbContention | null;
  plan_cache: PlanCacheHealth | null;
  /** Recent trend per headline scalar — the sparkline source. Always present
   *  (empty lists when the monitor hasn't captured a trend yet). */
  series: VitalSeries;
};

/** The most-recent deep-vitals sample of each surface the background monitor
 *  has persisted for this server. Read-only — reads the local telemetry store,
 *  never the live server. Returns `has_data:false` (200, not an error) when the
 *  monitor hasn't captured anything for this server yet. */
export async function fetchVitals(info: ConnectionInfo): Promise<DeepVitals> {
  const r = await fetch(`${BASE}/monitor/vitals`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `vitals fetch failed (${r.status})`);
  }
  return r.json() as Promise<DeepVitals>;
}

// ---------- threshold alerting --------------------------------------------

export type AlertSeverity = "info" | "warning" | "critical";
export type Comparator = "gt" | "ge" | "lt" | "le";
export type WebhookFormat = "generic" | "slack" | "teams";

/** A persisted fired-alert (a threshold breach Sentinel recorded). */
export type FiredAlert = {
  id: number;
  instance_name: string;
  fired_at: string; // RFC3339
  rule_id: string;
  metric: string;
  value: number;
  threshold: number;
  severity: AlertSeverity;
  message: string;
  notified: boolean;
};

/** A configurable alert threshold. `threshold` is tagged: a fixed number, or
 *  the dynamic PLE floor (which derives from the buffer-pool size at runtime). */
export type AlertThreshold =
  | { kind: "fixed"; value: number }
  | { kind: "ple_floor_per4_gb"; min_floor: number };

export type AlertRule = {
  id: string;
  metric: string;
  comparator: Comparator;
  threshold: AlertThreshold;
  severity: AlertSeverity;
  enabled: boolean;
  source: string;
};

export type AlertConfig = {
  webhook_url: string | null;
  webhook_format: WebhookFormat;
  cooldown_secs: number;
  rules: AlertRule[];
};

/** Recent fired alerts, newest first. Returns `[]` (not an error) when the
 *  monitor store doesn't exist yet or nothing has fired. */
export async function fetchAlerts(limit = 50): Promise<FiredAlert[]> {
  const r = await fetch(`${BASE}/alerts?limit=${limit}`);
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `alerts fetch failed (${r.status})`);
  }
  const j = (await r.json()) as { alerts: FiredAlert[] };
  return j.alerts ?? [];
}

/** The current alerting config (webhook + armed rules). Falls back to the
 *  grounded default rule set server-side when none has been saved yet. */
export async function getAlertConfig(): Promise<AlertConfig> {
  const r = await fetch(`${BASE}/alerts/config`);
  if (!r.ok) throw new Error(`alert config fetch failed (${r.status})`);
  return r.json() as Promise<AlertConfig>;
}

/** Save the alerting config. If the monitor is running it hot-reloads so the
 *  new thresholds take effect immediately. */
export async function setAlertConfig(cfg: AlertConfig): Promise<{ ok: boolean; reloaded: boolean }> {
  const r = await fetch(`${BASE}/alerts/config`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(cfg),
  });
  if (!r.ok) {
    const e = (await r.json().catch(() => ({}))) as { error?: string };
    throw new Error(e.error ?? `alert config save failed (${r.status})`);
  }
  return r.json() as Promise<{ ok: boolean; reloaded: boolean }>;
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

/**
 * Capture the ACTUAL execution plan — the backend runs the query inside a
 * transaction it always rolls back (so DML leaves no trace) and refuses
 * destructive / DDL / EXEC batches. Returns the ShowPlanXML (with real row
 * counts + runtime). Throws with the server's reason on refusal or error.
 */
export async function actualPlan(info: ConnectionInfo, sql: string): Promise<string> {
  const r = await fetch(`${BASE}/plan/actual`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...info, sql }),
  });
  const json = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error((json as any).error ?? `actual plan failed (${r.status})`);
  return (json as any).plan_xml as string;
}

export type QStoreStatus = { enabled: boolean; state: string; capture_mode: string; can_alter: boolean };

/** Query Store config for the connected database (capture_mode AUTO|ALL|NONE). */
export async function qstoreStatus(info: ConnectionInfo): Promise<QStoreStatus> {
  const r = await fetch(`${BASE}/qstore/status`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(info),
  });
  const json = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error((json as any).error ?? `query store status failed (${r.status})`);
  return json as QStoreStatus;
}

export type SlowQuery = {
  query_id: number;
  sql_text: string;
  executions: number;
  avg_duration_ms: number;
  max_duration_ms: number;
  avg_cpu_ms: number;
  avg_logical_reads: number;
};

/** Top long-running queries from the engine's captured workload history,
 *  ranked by average duration. Read-only telemetry: reads the engine's
 *  persisted query stats only — never executes the queries or reads table rows. */
export async function qstoreTop(info: ConnectionInfo, limit = 25): Promise<SlowQuery[]> {
  const r = await fetch(`${BASE}/qstore/top`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...info, limit }),
  });
  const json = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error((json as any).error ?? `workload query failed (${r.status})`);
  return ((json as any).queries ?? []) as SlowQuery[];
}

/** Set the connected DB's Query Store capture mode (runs DDL — caller must
 *  preview + confirm first). mode is allowlisted server-side to AUTO|ALL|NONE. */
export async function qstoreSetCapture(info: ConnectionInfo, mode: "AUTO" | "ALL" | "NONE"): Promise<{ ok: boolean; message?: string }> {
  const r = await fetch(`${BASE}/qstore/capture`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...info, mode }),
  });
  const json = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error((json as any).error ?? `set capture mode failed (${r.status})`);
  return json as { ok: boolean; message?: string };
}

export type ParseDiagnostic = { number: number; line: number; message: string };
export type ValidateResult = { ok: boolean; diagnostics: ParseDiagnostic[] };

/** Engine-checked "Parse" of a T-SQL batch (SET PARSEONLY
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
      const dataParts: string[] = [];
      for (const line of frame.split("\n")) {
        if (line.startsWith("event:")) evt = line.slice(6).trim();
        else if (line.startsWith("data:")) dataParts.push(line.slice(5).replace(/^ /, ""));
      }
      // Per the SSE spec an event's payload is its `data:` fields joined by "\n".
      // A content delta containing newlines arrives as several `data:` lines, so we
      // MUST rejoin with "\n" — concatenating with "" silently destroyed every code
      // block, heading, list and table in AI responses (turned them into one wall).
      const data = dataParts.join("\n");
      if (evt === "done") return;
      if (evt === "error") throw new Error(data || "stream error");
      if (data) onToken(data);
    }
  }
}
