/**
 * Analysis-run history.
 *
 * Records Analyze-editor runs (ad-hoc) to `/api/logs/analysis` so users have a
 * durable timeline of what they've analyzed, against which server/database,
 * with what plan cost, and the per-rule findings. Mirrors a small recent-N
 * list in memory for the History panel.
 */

import type { AnalysisReport } from "../types";

export type RunMode = "adhoc" | "database_scan";

export interface RunEntry {
  id: string;
  occurred_at: string;
  server_name: string | null;
  database_name: string | null;
  mode: RunMode;
  sql_hash: string | null;
  sql_preview: string | null;
  server_version: number | null;
  findings_total: number;
  findings_critical: number;
  findings_error: number;
  findings_warning: number;
  findings_info: number;
  plan_attached: boolean;
  plan_subtree_cost: number | null;
  plan_op_count: number | null;
  duration_ms: number | null;
}

const SUBS = new Set<() => void>();
let CACHE: RunEntry[] = [];

function notify() { for (const fn of SUBS) fn(); }
export function getAll(): RunEntry[] { return CACHE; }
export function subscribe(fn: () => void): () => void { SUBS.add(fn); return () => SUBS.delete(fn); }

async function sha16(s: string): Promise<string> {
  const data = new TextEncoder().encode(s);
  const buf = await crypto.subtle.digest("SHA-256", data);
  return Array.from(new Uint8Array(buf)).slice(0, 8).map((b) => b.toString(16).padStart(2, "0")).join("");
}

export async function record(args: {
  server_name: string | null;
  database_name: string | null;
  mode: RunMode;
  sql: string;
  server_version: number | null;
  report: AnalysisReport;
  plan_subtree_cost?: number | null;
  plan_op_count?: number | null;
  duration_ms?: number | null;
}): Promise<void> {
  const { sql, report } = args;
  const findings = report.findings ?? [];
  const sev = { critical: 0, error: 0, warning: 0, info: 0 } as Record<string, number>;
  for (const f of findings) sev[f.severity] = (sev[f.severity] ?? 0) + 1;
  const id = crypto.randomUUID();
  const body = {
    id,
    server_name: args.server_name,
    database_name: args.database_name,
    mode: args.mode,
    sql_hash: sql ? await sha16(sql) : null,
    sql_preview: sql ? sql.slice(0, 500) : null,
    server_version: args.server_version,
    findings_total: findings.length,
    findings_critical: sev.critical,
    findings_error: sev.error,
    findings_warning: sev.warning,
    findings_info: sev.info,
    plan_attached: !!args.plan_subtree_cost,
    plan_subtree_cost: args.plan_subtree_cost ?? null,
    plan_op_count: args.plan_op_count ?? null,
    duration_ms: args.duration_ms ?? null,
    findings: findings.map((f) => ({
      rule_id: f.rule,
      severity: f.severity,
      line_no: f.location?.line ?? null,
      col_no: f.location?.col ?? null,
      message: f.message,
      recommendation: f.recommendation ?? null,
    })),
  };
  try {
    await fetch("/api/logs/analysis", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  } catch {
    // best-effort; the in-memory cache still records
  }
  // Local cache (no need to refetch)
  const entry: RunEntry = {
    id,
    occurred_at: new Date().toISOString(),
    server_name: args.server_name,
    database_name: args.database_name,
    mode: args.mode,
    sql_hash: body.sql_hash,
    sql_preview: body.sql_preview,
    server_version: args.server_version,
    findings_total: findings.length,
    findings_critical: sev.critical,
    findings_error: sev.error,
    findings_warning: sev.warning,
    findings_info: sev.info,
    plan_attached: body.plan_attached,
    plan_subtree_cost: args.plan_subtree_cost ?? null,
    plan_op_count: args.plan_op_count ?? null,
    duration_ms: args.duration_ms ?? null,
  };
  CACHE = [entry, ...CACHE].slice(0, 200);
  notify();
}

export async function hydrate(filters?: { server?: string; database?: string }): Promise<void> {
  const params = new URLSearchParams({ limit: "200" });
  if (filters?.server) params.set("server", filters.server);
  if (filters?.database) params.set("database", filters.database);
  try {
    const r = await fetch(`/api/logs/analysis?${params}`);
    if (!r.ok) return;
    const body = (await r.json()) as { runs: RunEntry[] };
    CACHE = body.runs;
    notify();
  } catch { /* offline */ }
}

export async function fetchFindings(runId: string): Promise<Array<{
  rule: string; severity: string; line: number | null; col: number | null;
  message: string; recommendation: string | null;
}>> {
  try {
    const r = await fetch(`/api/logs/analysis/findings?id=${encodeURIComponent(runId)}`);
    if (!r.ok) return [];
    const body = await r.json() as { findings: Array<{rule:string;severity:string;line:number|null;col:number|null;message:string;recommendation:string|null}> };
    return body.findings;
  } catch { return []; }
}
