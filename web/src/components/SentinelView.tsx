import { useCallback, useEffect, useState } from "react";
import type { SqlConnectionConfig } from "../store/persist";

// ---------- Wire types (mirror crates/sentinel/src/report.rs) -------------

interface PainSummaryDto {
  top_wait_type: string | null;
  top_wait_time_ms: number;
  deadlock_count: number;
  blocking_incidents: number;
}

interface TopQueryDto {
  query_id: number;
  plan_id: number;
  total_duration_ms: number;
  executions: number;
  avg_duration_ms: number;
  query_sql_text?: string | null;
}

interface RegressionDto {
  query_id: number;
  baseline_duration_ms: number;
  current_duration_ms: number;
  delta_pct: number;
}

interface UnusedIndexDto {
  db_name: string;
  schema_name: string;
  table_name: string;
  index_name: string;
  updates_in_window: number;
}

interface WeeklyReport {
  window_from: string;
  window_to: string;
  instances: number;
  pain: PainSummaryDto;
  top_queries: TopQueryDto[];
  regressions: RegressionDto[];
  unused_indexes: UnusedIndexDto[];
}

interface SentinelStatus {
  running: boolean;
  db_path: string;
  instances: number;
}

const BASE = "/api";
const DAYS = 7;

function fmtMs(ms: number): string {
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)} min`;
  if (ms >= 1_000) return `${(ms / 1_000).toFixed(2)} s`;
  return `${ms} ms`;
}

function fmtTs(s: string): string {
  try {
    return new Date(s).toISOString().slice(0, 16).replace("T", " ") + " UTC";
  } catch {
    return s;
  }
}

function downloadBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function SentinelView({ conn }: { conn: SqlConnectionConfig }) {
  const [status, setStatus] = useState<SentinelStatus | null>(null);
  const [report, setReport] = useState<WeeklyReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setErr(null);
    try {
      const [s, r] = await Promise.all([
        fetch(`${BASE}/sentinel/status`).then((x) => x.json()) as Promise<SentinelStatus>,
        fetch(`${BASE}/sentinel/report?days=${DAYS}`).then((x) => x.json()) as Promise<WeeklyReport>,
      ]);
      setStatus(s);
      setReport(r);
    } catch (e: any) {
      setErr(e?.message ?? String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  async function start() {
    if (!conn.server) {
      setErr("Configure a SQL Server connection first (CONN workspace).");
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      const body = {
        instances: [
          {
            name: conn.server,
            conn: {
              server: conn.server,
              database: conn.database || null,
              user: conn.auth_mode === "sql" ? conn.user || null : null,
              password: conn.auth_mode === "sql" ? conn.password || null : null,
              trust_cert: conn.trust_cert,
            },
          },
        ],
      };
      const r = await fetch(`${BASE}/sentinel/start`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      const json = await r.json().catch(() => ({}));
      if (!r.ok || json.ok === false) {
        throw new Error(json.error ?? `start failed (${r.status})`);
      }
      await refresh();
    } catch (e: any) {
      setErr(e?.message ?? String(e));
    } finally {
      setBusy(false);
    }
  }

  async function stop() {
    setBusy(true);
    setErr(null);
    try {
      await fetch(`${BASE}/sentinel/stop`, { method: "POST" });
      await refresh();
    } catch (e: any) {
      setErr(e?.message ?? String(e));
    } finally {
      setBusy(false);
    }
  }

  async function downloadHtml() {
    const r = await fetch(`${BASE}/sentinel/report.html?days=${DAYS}`);
    const blob = await r.blob();
    downloadBlob(blob, `sqlopt-sentinel-${new Date().toISOString().slice(0, 10)}.html`);
  }

  async function downloadJson() {
    const r = await fetch(`${BASE}/sentinel/report?days=${DAYS}`);
    const blob = await r.blob();
    downloadBlob(blob, `sqlopt-sentinel-${new Date().toISOString().slice(0, 10)}.json`);
  }

  const running = status?.running ?? false;
  const mono: React.CSSProperties = { font: "12px var(--f-mono, ui-monospace, Menlo, monospace)" };
  const numCell: React.CSSProperties = { ...mono, textAlign: "right", whiteSpace: "nowrap" };

  return (
    <div className="pane-body" style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      {/* ── Status / controls bar ───────────────────────── */}
      <div
        className="pane-title"
        style={{ position: "sticky", top: 0, zIndex: 2, alignItems: "center" }}
      >
        <div className="label" style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <span
            className={`dot ${running ? "ok" : "err"}`}
            style={{
              display: "inline-block",
              width: 8,
              height: 8,
              borderRadius: "50%",
              background: running ? "var(--ok, #5dd39e)" : "var(--crit, #ff3a4a)",
            }}
          />
          <b>SENTINEL</b>
          <span style={mono}>{running ? "RUNNING" : "STOPPED"}</span>
          <span style={{ ...mono, color: "var(--text-dim)" }}>
            · {status?.instances ?? 0} instance(s)
          </span>
          {status?.db_path && (
            <span style={{ ...mono, color: "var(--text-dim)", marginLeft: 6 }}>
              · {status.db_path}
            </span>
          )}
        </div>
        <div className="ops">
          <button onClick={start} disabled={busy || running} title="Start the daemon for the current SQL Server connection">
            START
          </button>
          <button onClick={stop} disabled={busy || !running} title="Stop the daemon">
            STOP
          </button>
          <button onClick={refresh} disabled={busy} title="Reload status and report">
            REFRESH
          </button>
          <button onClick={downloadHtml} title="Download the report as a self-contained HTML file">
            DOWNLOAD HTML
          </button>
          <button onClick={downloadJson} title="Download the raw JSON report">
            DOWNLOAD JSON
          </button>
        </div>
      </div>

      {err && (
        <div
          style={{
            padding: "8px 14px",
            background: "rgba(255,58,74,0.08)",
            borderBottom: "1px solid var(--line)",
            color: "var(--crit, #ff3a4a)",
            font: "11px var(--f-mono, ui-monospace, Menlo, monospace)",
          }}
        >
          {err}
        </div>
      )}

      {/* ── Window + headline pain ─────────────────────── */}
      <div style={{ padding: "14px 18px", borderBottom: "1px solid var(--line)" }}>
        <div style={{ ...mono, color: "var(--text-dim)", marginBottom: 8 }}>
          window&nbsp;
          {report ? `${fmtTs(report.window_from)} → ${fmtTs(report.window_to)}` : "—"}
        </div>
        <div style={{ display: "grid", gridTemplateColumns: "repeat(4, minmax(140px, 1fr))", gap: 10 }}>
          <Stat label="TOP WAIT" value={report?.pain.top_wait_type ?? "—"} sub={fmtMs(report?.pain.top_wait_time_ms ?? 0)} />
          <Stat label="DEADLOCKS" value={String(report?.pain.deadlock_count ?? 0)} />
          <Stat label="BLOCKING INCIDENTS" value={String(report?.pain.blocking_incidents ?? 0)} />
          <Stat label="INSTANCES" value={String(report?.instances ?? 0)} />
        </div>
      </div>

      {/* ── Top queries ────────────────────────────────── */}
      <Section title={`TOP ${report?.top_queries.length ?? 0} QUERIES BY TOTAL DURATION`}>
        {report && report.top_queries.length > 0 ? (
          <table className="findings" style={{ width: "100%", borderCollapse: "collapse", tableLayout: "fixed" }}>
            <thead>
              <Th cols={["Query", "SQL text", "Total", "Executions", "Avg"]} aligns={["l", "l", "r", "r", "r"]} />
            </thead>
            <tbody>
              {report.top_queries.map((q, i) => (
                <tr key={i}>
                  <td style={{ ...mono, width: 64 }}>{q.query_id}</td>
                  <td style={{ ...mono, whiteSpace: "pre-wrap", wordBreak: "break-word", color: "var(--text)" }}>
                    {q.query_sql_text ?? "—"}
                  </td>
                  <td style={{ ...numCell, width: 90 }}>{fmtMs(q.total_duration_ms)}</td>
                  <td style={{ ...numCell, width: 90 }}>{q.executions.toLocaleString()}</td>
                  <td style={{ ...numCell, width: 90 }}>{fmtMs(q.avg_duration_ms)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <Empty msg="No Query Store rows in window. Enable Query Store on the target database, then wait for the next poll." />
        )}
      </Section>

      {/* ── Regressions ────────────────────────────────── */}
      <Section title="REGRESSIONS (≥2× SLOWER VS. BASELINE HALF-WINDOW)">
        {report && report.regressions.length > 0 ? (
          <table className="findings" style={{ width: "100%", borderCollapse: "collapse" }}>
            <thead>
              <Th cols={["Query", "Baseline", "Current", "Δ"]} aligns={["l", "r", "r", "r"]} />
            </thead>
            <tbody>
              {report.regressions.map((r, i) => (
                <tr key={i}>
                  <td style={mono}>{r.query_id}</td>
                  <td style={numCell}>{fmtMs(r.baseline_duration_ms)}</td>
                  <td style={numCell}>{fmtMs(r.current_duration_ms)}</td>
                  <td style={{ ...numCell, color: "var(--crit, #ff3a4a)" }}>+{r.delta_pct.toFixed(0)}%</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <Empty msg="No regressions detected in window." />
        )}
      </Section>

      {/* ── Unused indexes ────────────────────────────── */}
      <Section title="INDEXES ACCUMULATING WRITES WITH ZERO READS">
        {report && report.unused_indexes.length > 0 ? (
          <table className="findings" style={{ width: "100%", borderCollapse: "collapse" }}>
            <thead>
              <Th cols={["Table", "Index", "Writes"]} aligns={["l", "l", "r"]} />
            </thead>
            <tbody>
              {report.unused_indexes.map((u, i) => (
                <tr key={i}>
                  <td style={mono}>{`${u.db_name}.${u.schema_name}.${u.table_name}`}</td>
                  <td style={mono}>{u.index_name}</td>
                  <td style={numCell}>{u.updates_in_window.toLocaleString()}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <Empty msg="No fully-unused indexes in window." />
        )}
      </Section>
    </div>
  );
}

function Stat({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div
      style={{
        border: "1px solid var(--line)",
        padding: "10px 12px",
        background: "var(--bg-elev, transparent)",
      }}
    >
      <div
        style={{
          font: "10px var(--f-mono, ui-monospace, Menlo, monospace)",
          color: "var(--text-dim)",
          letterSpacing: "0.1em",
        }}
      >
        {label}
      </div>
      <div
        style={{
          font: "16px var(--f-mono, ui-monospace, Menlo, monospace)",
          color: "var(--text)",
          marginTop: 4,
          wordBreak: "break-all",
        }}
      >
        {value}
      </div>
      {sub && (
        <div
          style={{
            font: "11px var(--f-mono, ui-monospace, Menlo, monospace)",
            color: "var(--text-dim)",
            marginTop: 2,
          }}
        >
          {sub}
        </div>
      )}
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{ borderBottom: "1px solid var(--line)" }}>
      <div
        className="pane-title"
        style={{ position: "sticky", top: 0, zIndex: 1, background: "var(--bg, #0a0d12)" }}
      >
        <div className="label">
          <b>{title}</b>
        </div>
      </div>
      <div style={{ padding: "0 0 6px" }}>{children}</div>
    </div>
  );
}

function Th({ cols, aligns }: { cols: string[]; aligns: ("l" | "r")[] }) {
  return (
    <tr>
      {cols.map((c, i) => (
        <th
          key={c}
          style={{
            textAlign: aligns[i] === "r" ? "right" : "left",
            padding: "6px 14px",
            borderBottom: "1px solid var(--line)",
            font: "10px var(--f-mono, ui-monospace, Menlo, monospace)",
            color: "var(--text-dim)",
            letterSpacing: "0.1em",
            fontWeight: 400,
          }}
        >
          {c}
        </th>
      ))}
    </tr>
  );
}

function Empty({ msg }: { msg: string }) {
  return (
    <div
      style={{
        padding: "14px 18px",
        font: "12px var(--f-mono, ui-monospace, Menlo, monospace)",
        color: "var(--text-dim)",
      }}
    >
      {msg}
    </div>
  );
}
