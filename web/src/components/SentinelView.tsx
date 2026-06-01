import { useCallback, useEffect, useState } from "react";
import type { SqlConnectionConfig } from "../store/persist";
import { LiveMonitor } from "./LiveMonitor";
import * as backend from "../api/backend";
import type { QStoreStatus } from "../api/backend";

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
  last_run_ms?: number | null;
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
  recent_queries?: TopQueryDto[];
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

/** Relative "x ago" for a unix-ms last-execution time; "—" when unknown. */
function fmtLastRun(ms?: number | null): string {
  if (ms == null) return "—";
  const diff = Date.now() - ms;
  if (diff < 0) return "just now";
  const s = Math.floor(diff / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

function fmtTsMs(ms?: number | null): string {
  if (ms == null) return "unknown";
  try { return new Date(ms).toISOString().slice(0, 19).replace("T", " ") + " UTC"; } catch { return "unknown"; }
}

function fmtTs(s: string): string {
  try {
    return new Date(s).toISOString().slice(0, 16).replace("T", " ") + " UTC";
  } catch {
    return s;
  }
}

// Filesystem-safe local timestamp `YYYY-MM-DD_HHMMSS` so each download is a
// distinct file (no browser "(1)", "(2)" suffixes) and sorts chronologically.
// No colons — keeps it valid on Windows.
function fileStamp(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return (
    `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}` +
    `_${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`
  );
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

export function SentinelView({ conn, onAnalyzeSql }: { conn: SqlConnectionConfig; onAnalyzeSql?: (sql: string) => void }) {
  const [tab, setTab] = useState<"live" | "report">("live");
  const [querySort, setQuerySort] = useState<"duration" | "recent">("duration");
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

  // Downloads honor the selected sort so the file matches the on-screen view.
  async function downloadHtml() {
    const r = await fetch(`${BASE}/sentinel/report.html?days=${DAYS}&sort=${querySort}`);
    const blob = await r.blob();
    downloadBlob(blob, `dbopt-sentinel-${fileStamp()}.html`);
  }

  async function downloadJson() {
    const r = await fetch(`${BASE}/sentinel/report?days=${DAYS}&sort=${querySort}`);
    const blob = await r.blob();
    downloadBlob(blob, `dbopt-sentinel-${fileStamp()}.json`);
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
        <div className="label" style={{ display: "flex", alignItems: "center", gap: 10, minWidth: 0 }}>
          <span
            className={`dot ${running ? "ok" : "err"}`}
            style={{
              display: "inline-block",
              width: 8,
              height: 8,
              borderRadius: "50%",
              flex: "none",
              background: running ? "var(--ok, #5dd39e)" : "var(--crit, #ff3a4a)",
            }}
          />
          <b>SENTINEL</b>
          <span style={mono}>{running ? "RUNNING" : "STOPPED"}</span>
          {busy && <span style={{ ...mono, color: "var(--accent, #d4ff4e)" }}>· working…</span>}
          <span style={{ ...mono, color: "var(--text-dim)" }}>
            · {status?.instances ?? 0} instance(s)
          </span>
        </div>
        {/* Toggle stays in the header; daemon actions move to their own row. */}
        <span className="live-tabs">
          <button className={tab === "live" ? "active" : ""} onClick={() => setTab("live")} title="Real-time server vitals — live pulse">
            ● LIVE
          </button>
          <button className={tab === "report" ? "active" : ""} onClick={() => setTab("report")} title="Accumulated weekly pain report from the background daemon">
            REPORT
          </button>
        </span>
      </div>

      {/* ── Daemon actions row (report mode) — kept off the header so the
            controls never crowd the title or truncate the db path. ── */}
      {tab === "report" && (
        <div className="watch-actions">
          {status?.db_path && (
            <span className="watch-dbpath" title={status.db_path}>{status.db_path}</span>
          )}
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
      )}

      {tab === "live" && <LiveMonitor conn={conn} />}

      {tab === "report" && (<>

      <QStoreCapture conn={conn} />

      {err && (
        <div
          style={{
            padding: "8px 14px",
            background: "var(--crit-glow, rgba(255,58,74,0.08))",
            borderBottom: "1px solid var(--line)",
            color: "var(--crit, #ff3a4a)",
            font: "11px var(--f-mono, ui-monospace, Menlo, monospace)",
          }}
        >
          {err}
        </div>
      )}

      {/* ── First-run empty state ──────────────────────── */}
      {!running && !report && !busy && (
        <div style={{ padding: "22px 18px", borderBottom: "1px solid var(--line)" }}>
          <div style={{ ...mono, fontSize: 13, color: "var(--text)", marginBottom: 6 }}>
            Sentinel isn't running yet.
          </div>
          <div style={{ ...mono, color: "var(--text-dim)", lineHeight: 1.6, maxWidth: 620 }}>
            Click <b style={{ color: "var(--accent, #d4ff4e)" }}>START</b> to begin sampling your
            connected SQL Server on demand. It polls Query Store, wait stats, deadlocks,
            live blocking, index usage, and table sizes into a local SQLite time-series, then rolls
            them up into the pain report below. Polling runs in the background while it's started —
            leave this tab any time. It captures data and writes a report you read yourself; it does
            not page or alert.
          </div>
          <div style={{ ...mono, color: "var(--text-dim)", marginTop: 10, opacity: 0.8 }}>
            Uses the connection from the Connection tab (SQL authentication).
          </div>
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

      {/* ── Top queries (toggle: by total duration ⇄ by last run) ── */}
      {(() => {
        const list = querySort === "recent"
          ? (report?.recent_queries ?? [])
          : (report?.top_queries ?? []);
        return (
          <div style={{ borderBottom: "1px solid var(--line)" }}>
            <div className="pane-title" style={{ position: "sticky", top: 0, zIndex: 1, background: "var(--bg-panel)", alignItems: "center" }}>
              <div className="label">
                <b>TOP QUERIES</b> {querySort === "recent" ? "by last run" : "by total duration"} · {list.length}
              </div>
              <span className="live-tabs">
                <button className={querySort === "duration" ? "active" : ""} onClick={() => setQuerySort("duration")} title="Heaviest by total time spent in window">BY DURATION</button>
                <button className={querySort === "recent" ? "active" : ""} onClick={() => setQuerySort("recent")} title="Most recently executed">BY LAST RUN</button>
              </span>
            </div>
            {list.length > 0 ? (
              <table style={{ width: "100%", borderCollapse: "collapse", tableLayout: "fixed" }}>
                <colgroup>
                  <col style={{ width: 56 }} />
                  <col />
                  <col style={{ width: 96 }} />
                  <col style={{ width: 92 }} />
                  <col style={{ width: 80 }} />
                  <col style={{ width: 92 }} />
                  <col style={{ width: 138 }} />
                </colgroup>
                <thead>
                  <Th cols={["Query", "SQL text", "Total", "Executions", "Avg", "Last run", "Actions"]} aligns={["l", "l", "r", "r", "r", "r", "r"]} />
                </thead>
                <tbody>
                  {list.map((q, i) => (
                    <QueryRow key={i} q={q} onAnalyzeSql={onAnalyzeSql} />
                  ))}
                </tbody>
              </table>
            ) : (
              <Empty msg="No Query Store rows in window. Enable Query Store on the target database, then wait for the next poll." />
            )}
          </div>
        );
      })()}

      {/* ── Regressions ────────────────────────────────── */}
      <Section title="REGRESSIONS (STATISTICAL OUTLIERS · Z-SCORE VS. ROLLING BASELINE)">
        {report && report.regressions.length > 0 ? (
          <table style={{ width: "100%", borderCollapse: "collapse" }}>
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
          <table style={{ width: "100%", borderCollapse: "collapse" }}>
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
      </>)}
    </div>
  );
}

function Stat({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div
      style={{
        border: "1px solid var(--line-strong)",
        padding: "12px 14px",
        background: "var(--bg-elev)",
      }}
    >
      <div
        style={{
          font: "10px var(--f-mono, ui-monospace, Menlo, monospace)",
          color: "var(--text-muted)",
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
        style={{ position: "sticky", top: 0, zIndex: 1, background: "var(--bg-panel)" }}
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
            color: "var(--text-muted)",
            letterSpacing: "0.1em",
            fontWeight: 500,
            textTransform: "uppercase",
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

/**
 * One Top-Queries row with its own copy/analyze actions. The captured T-SQL is
 * otherwise only reachable via the hover tooltip — these buttons let you grab it
 * for analysis: COPY puts the statement on the clipboard (works for read-only
 * logins too), ANALYZE → loads it straight into the Analyze editor.
 *
 * Note: Query Store stores statement-level text and the poller keeps the first
 * 1000 chars, so very long batches are copied truncated (we surface that in the
 * button title rather than silently misleading).
 */
function QueryRow({ q, onAnalyzeSql }: { q: TopQueryDto; onAnalyzeSql?: (sql: string) => void }) {
  const [copied, setCopied] = useState(false);
  const mono: React.CSSProperties = { font: "12px var(--f-mono, ui-monospace, Menlo, monospace)" };
  const numCell: React.CSSProperties = { ...mono, textAlign: "right", whiteSpace: "nowrap" };
  const raw = (q.query_sql_text ?? "").trim();
  const hasText = raw.length > 0;
  const maybeTruncated = raw.length >= 1000;

  async function copy() {
    if (!hasText) return;
    try {
      await navigator.clipboard?.writeText(raw);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      /* clipboard may be blocked (non-secure context) — silent, button just won't tick */
    }
  }

  return (
    <tr>
      <td style={mono}>{q.query_id}</td>
      <td style={{ ...mono, color: "var(--text)" }}>
        <div
          title={hasText ? raw : undefined}
          style={{
            display: "-webkit-box",
            WebkitLineClamp: 2,
            WebkitBoxOrient: "vertical",
            overflow: "hidden",
            wordBreak: "break-word",
            cursor: hasText ? "help" : "default",
          }}
        >
          {hasText ? raw.replace(/\s+/g, " ") : "—"}
        </div>
      </td>
      <td style={numCell}>{fmtMs(q.total_duration_ms)}</td>
      <td style={numCell}>{q.executions.toLocaleString()}</td>
      <td style={numCell}>{fmtMs(q.avg_duration_ms)}</td>
      <td style={numCell} title={fmtTsMs(q.last_run_ms)}>{fmtLastRun(q.last_run_ms)}</td>
      <td>
        <div className="q-actions">
          <button
            className={`q-act${copied ? " copied" : ""}`}
            disabled={!hasText}
            onClick={copy}
            title={maybeTruncated ? "Copy the captured T-SQL (truncated to the first 1000 chars)" : "Copy the captured T-SQL to the clipboard"}
          >
            {copied ? "COPIED ✓" : "COPY"}
          </button>
          {onAnalyzeSql && (
            <button
              className="q-act"
              disabled={!hasText}
              onClick={() => onAnalyzeSql(raw)}
              title="Load this query into the Analyze editor"
            >
              ANALYZE →
            </button>
          )}
        </div>
      </td>
    </tr>
  );
}

/**
 * Query Store capture-mode control. AUTO (default) lets SQL Server skip the
 * cheapest/rarest queries; ALL captures every statement. Switching ALL is a
 * DDL change to the connected database, so we PREVIEW the exact statement and
 * require an explicit Apply (Safe-Apply) — never silent. Per-database.
 */
function QStoreCapture({ conn }: { conn: SqlConnectionConfig }) {
  const [status, setStatus] = useState<QStoreStatus | null>(null);
  const [pending, setPending] = useState<"AUTO" | "ALL" | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const connected = !!conn.server && !!conn.database;

  const payload = {
    server: conn.server,
    database: conn.database || undefined,
    user: conn.auth_mode === "sql" ? conn.user : undefined,
    password: conn.auth_mode === "sql" ? conn.password : undefined,
    trust_cert: conn.trust_cert,
  };

  const load = useCallback(async () => {
    if (!connected) return;
    try {
      setStatus(await backend.qstoreStatus(payload as any));
      setErr(null);
    } catch (e: any) {
      setErr(e?.message ?? String(e));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conn.server, conn.database, conn.user, conn.password, conn.trust_cert]);

  useEffect(() => { load(); }, [load]);

  async function apply(mode: "AUTO" | "ALL") {
    setBusy(true); setErr(null);
    try {
      await backend.qstoreSetCapture(payload as any, mode);
      setPending(null);
      await load();
    } catch (e: any) {
      const raw = e?.message ?? String(e);
      // Turn the engine's permission denial into a plain, actionable message.
      setErr(/permission|denied|alter database/i.test(raw)
        ? `Couldn't change it — your login lacks ALTER on ${conn.database} (needs db_owner / sysadmin). Use COPY to hand the statement to a DBA.`
        : `Couldn't change capture mode: ${raw}`);
    } finally {
      setBusy(false);
    }
  }

  if (!connected) return null;

  const mode = status?.capture_mode ?? "—";
  const canAlter = !!status?.can_alter;
  const stmt = (m: string) => `ALTER DATABASE CURRENT SET QUERY_STORE (QUERY_CAPTURE_MODE = ${m})`;
  const copy = (m: "AUTO" | "ALL") => { try { navigator.clipboard?.writeText(stmt(m) + ";"); } catch { /* clipboard may be blocked */ } };
  const tryToggle = (m: "AUTO" | "ALL") => {
    if (mode === m) return;
    if (!canAlter) { setErr(`Your login can't change this — it needs ALTER on ${conn.database} (db_owner / sysadmin). Use COPY to hand the statement to a DBA.`); return; }
    setPending(m);
  };

  return (
    <div className="qstore-strip">
      <div className="qstore-row">
        <span className="qstore-label">QUERY STORE CAPTURE</span>
        <span className="live-tabs">
          <button className={mode === "AUTO" ? "active" : ""} disabled={busy || !canAlter} onClick={() => tryToggle("AUTO")}>AUTO</button>
          <button className={mode === "ALL" ? "active" : ""} disabled={busy || !canAlter} onClick={() => tryToggle("ALL")}>ALL</button>
        </span>
        <span className="qstore-note">
          {status && !status.enabled
            ? "Query Store is OFF for this database."
            : mode === "ALL"
            ? "Full capture — every statement is recorded (higher overhead). AUTO is the recommended default."
            : "AUTO (recommended default) — recurring/expensive queries are captured; one-off ad-hoc queries are skipped."}
        </span>
      </div>
      <div className="qstore-perm">
        {canAlter ? (
          <>Viewing the mode needs only read access; <b>changing</b> it needs <code>ALTER</code> on the database
          (<code>db_owner</code> / <code>sysadmin</code>).</>
        ) : (
          <>Your login can <b>view</b> the mode but not change it (needs <code>ALTER</code> — <code>db_owner</code> / <code>sysadmin</code>).{" "}
          <button className="qstore-copy-link" onClick={() => copy("ALL")} title="Copy the ALTER statement to enable full capture">Copy the “enable full capture” statement for a DBA →</button></>
        )}
      </div>
      {err && <div className="qstore-err">{err}</div>}
      {pending && (
        <div className="qstore-confirm">
          <span>Apply this change to <b>{conn.database}</b>?</span>
          <code>{stmt(pending)}</code>
          <div className="qstore-confirm-ops">
            <button className="primary" disabled={busy} onClick={() => apply(pending)}>{busy ? "APPLYING…" : "APPLY"}</button>
            <button
              disabled={busy}
              onClick={() => copy(pending)}
              title="Copy the statement so a DBA can run it"
            >
              COPY
            </button>
            <button disabled={busy} onClick={() => setPending(null)}>CANCEL</button>
          </div>
        </div>
      )}
    </div>
  );
}
