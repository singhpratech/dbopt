import { useCallback, useEffect, useState } from "react";
import type { SqlConnectionConfig, UiPrefs } from "../store/persist";
import * as backend from "../api/backend";
import type { HealthReport, Issue, IssueSeverity } from "../api/backend";

/**
 * The HEALTH workspace — the one-screen front-door (default landing).
 *
 * Asks the backend for ONE engine-neutral, server-side aggregated HealthReport
 * (POST /api/health/db) that fuses advisor recs + static findings + sentinel
 * pain into a flat, pre-ranked Issue[]. We render the report only; all scoring
 * and ranking is done server-side, so we paint cards as-is.
 *
 * Four states (+ populated): not-connected, scanning, healthy/empty, error.
 * Reuses AdvisorPanel .pill/.advisor-card/.ddl-copy/.empty CSS for consistency.
 */
export function HealthOverview({
  conn,
  ui,
  setUi,
}: {
  conn: SqlConnectionConfig;
  ui: UiPrefs;
  setUi: (u: UiPrefs) => void;
}) {
  const [report, setReport] = useState<HealthReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const connected = !!conn.server;

  const scan = useCallback(async () => {
    if (!conn.server) return;
    setBusy(true);
    setErr(null);
    try {
      const info = {
        server: conn.server,
        database: conn.database || undefined,
        user: conn.auth_mode === "sql" ? conn.user : undefined,
        password: conn.auth_mode === "sql" ? conn.password : undefined,
        trust_cert: conn.trust_cert,
      };
      const r = await backend.getDbHealth(info);
      setReport(r);
    } catch (e: any) {
      setErr(e?.message ?? String(e));
      setReport(null);
    } finally {
      setBusy(false);
    }
    // conn fields are the only inputs; deliberately keyed below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conn.server, conn.database, conn.user, conn.password, conn.auth_mode, conn.trust_cert]);

  // Auto-fetch on mount + whenever the active server/database changes.
  useEffect(() => {
    if (conn.server) void scan();
    else {
      setReport(null);
      setErr(null);
    }
  }, [conn.server, conn.database, scan]);

  // The displayed grade collapses to "?" when we have nothing real to show.
  const grade = err ? "?" : !connected ? "?" : report?.grade ?? "?";
  const gradeClass = gradeChipClass(grade);

  return (
    <div className="advisor form">
      {/* ── 1) GRADE HEADER ───────────────────────────── */}
      <div className={`health-header${busy ? " scanning" : ""}`}>
        <div className={`health-grade ${gradeClass}`} title="overall health grade">
          <span className="health-score">{connected && report && !err ? report.score : "—"}</span>
          <span className={`pill ${gradeClass}`}>{grade}</span>
        </div>
        <div className="health-head-meta">
          <div className="health-status">
            {err
              ? "Error"
              : !connected
              ? "Not connected"
              : busy
              ? "Scanning…"
              : report
              ? report.status
              : "Idle"}
          </div>
          <div className="health-window">
            {report && !err
              ? `Window: last 7 days · scanned ${fmtTime(report.generated_at)}`
              : "aggregated database health · one-screen front-door"}
          </div>
        </div>
        <div className="form-actions health-actions">
          <button className="btn primary" onClick={() => void scan()} disabled={busy || !connected}>
            {busy ? "Scanning…" : "Refresh"}
          </button>
          {busy && <span className="advisor-spinner" aria-hidden />}
        </div>
      </div>

      {report?.is_learning && !err && (
        <div className="health-learning">
          Learning mode — DMV signal counters look freshly reset (post-restart). Absence of
          signal is not proof of health; the grade is provisional until a workload accumulates.
        </div>
      )}

      {/* ── State machine ─────────────────────────────── */}
      {!connected ? (
        <div className="empty">
          <div className="empty-card">
            <div className="empty-glyph">❤</div>
            <div className="empty-title">No connection</div>
            <div className="empty-hint">Configure a SQL Server connection first.</div>
            <div className="form-actions" style={{ justifyContent: "center" }}>
              <button className="btn primary" onClick={() => setUi({ ...ui, workspace: "connection" })}>
                Open CONN
              </button>
            </div>
          </div>
        </div>
      ) : busy && !report ? (
        <div className="form-status">
          Pulling DMV + sentinel health signals… this can take up to 30–90s.
        </div>
      ) : err ? (
        <div className="empty">
          <div className="empty-card health-err-card">
            <div className="empty-glyph">⚠</div>
            <div className="empty-title">Health scan failed</div>
            <div className="form-status err">{err}</div>
            <div className="form-actions" style={{ justifyContent: "center" }}>
              <button className="btn primary" onClick={() => void scan()} disabled={busy}>
                Retry
              </button>
            </div>
          </div>
        </div>
      ) : report ? (
        <>
          {/* ── 2) SIGNAL STRIP ─────────────────────────── */}
          <div className="health-signals">
            <Signal label="missing idx" value={report.signals.missing_indexes} />
            <Signal label="unused idx" value={report.signals.unused_indexes} />
            <Signal label="duplicate idx" value={report.signals.duplicate_indexes} />
            <Signal label="columnstore" value={report.signals.columnstore_candidates} />
            <Signal label="deadlocks" value={report.signals.deadlock_count} tone="crit" />
            <Signal label="blocking" value={report.signals.blocking_incidents} tone="warn" />
            <Signal
              label="top wait"
              value={
                report.signals.top_wait_type
                  ? `${report.signals.top_wait_type} · ${fmtMs(report.signals.top_wait_time_ms)}`
                  : "—"
              }
            />
            <Signal label="regressions" value={report.signals.regressed_queries} tone="warn" />
          </div>

          {/* ── 3) RANKED ISSUE LIST ────────────────────── */}
          {report.issues.length === 0 ? (
            <div className="empty">
              <div className="empty-card">
                <div className="empty-glyph">✦</div>
                <div className="empty-title">No actionable issues</div>
                <div className="empty-hint">
                  {report.is_learning
                    ? "Signal counters are still warming up. Re-check after the workload runs for a while."
                    : "No missing/unused/duplicate indexes or sentinel pain detected. Healthy."}
                </div>
              </div>
            </div>
          ) : (
            <div className="health-issue-list">
              {report.issues.map((iss) => (
                <IssueCard key={iss.id} iss={iss} ui={ui} setUi={setUi} />
              ))}
            </div>
          )}
        </>
      ) : null}
    </div>
  );
}

function Signal({
  label,
  value,
  tone,
}: {
  label: string;
  value: number | string;
  tone?: "crit" | "warn";
}) {
  const hot = typeof value === "number" && value > 0 && tone;
  return (
    <div className="health-signal">
      <span className="health-signal-k">{label}</span>
      <span className={`health-signal-v${hot ? ` ${tone}` : ""}`}>
        {typeof value === "number" ? value.toLocaleString() : value}
      </span>
    </div>
  );
}

function IssueCard({
  iss,
  ui,
  setUi,
}: {
  iss: Issue;
  ui: UiPrefs;
  setUi: (u: UiPrefs) => void;
}) {
  const [copied, setCopied] = useState(false);
  const [open, setOpen] = useState(false);

  async function copy() {
    if (!iss.fix_sql) return;
    try {
      await navigator.clipboard.writeText(iss.fix_sql);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      /* clipboard unavailable (insecure context) — silently ignore */
    }
  }

  const links = deepLinks(iss);

  return (
    <div className="advisor-card health-issue">
      <div className="advisor-card-head">
        <span className={`pill ${severityClass(iss.severity)}`}>{iss.severity}</span>
        <span className="advisor-kind">{kindLabel(iss.kind)}</span>
        <span className="advisor-title">{iss.title}</span>
        <span className="advisor-score" title="impact rank">
          {iss.impact_rank.toLocaleString()}
        </span>
      </div>
      <div className="advisor-object">
        <code>{iss.affected_object}</code>
      </div>

      {iss.rationale && (
        <>
          <button className="health-toggle" onClick={() => setOpen((o) => !o)}>
            {open ? "▾ rationale" : "▸ rationale"}
          </button>
          {open && <div className="advisor-rationale">{iss.rationale}</div>}
        </>
      )}

      {(iss.fix_sql || links.length > 0) && (
        <div className="health-issue-foot">
          {links.map((l) => (
            <button
              key={l.workspace}
              className="ddl-copy"
              onClick={() => setUi({ ...ui, workspace: l.workspace })}
              title={`Jump to the ${l.label} workspace`}
            >
              {l.label}
            </button>
          ))}
        </div>
      )}

      {iss.fix_sql && (
        <div className="ddl-wrap">
          <button className="ddl-copy" onClick={copy} title="Copy fix SQL to clipboard">
            {copied ? "Copied ✓" : "Copy"}
          </button>
          <pre className="ddl">{iss.fix_sql}</pre>
        </div>
      )}
    </div>
  );
}

/** Deep-link buttons by issue kind, routed via setUi (workspace switch). */
function deepLinks(iss: Issue): { workspace: UiPrefs["workspace"]; label: string }[] {
  switch (iss.kind) {
    case "missing_index":
    case "duplicate_index":
    case "columnstore_candidate":
      return [{ workspace: "advisor", label: "Open ADVISE" }];
    case "unused_index":
      return [
        { workspace: "advisor", label: "Open ADVISE" },
        { workspace: "indexes", label: "Open INDEX" },
      ];
    case "regression":
      return [{ workspace: "history", label: "Open RUNS" }];
    default:
      return [];
  }
}

/** critical/error → crit, warning → warn, info → dim (reuses .pill variants). */
function severityClass(s: IssueSeverity): string {
  switch (s) {
    case "critical":
    case "error":
      return "crit";
    case "warning":
      return "warn";
    case "info":
      return "dim";
  }
}

function gradeChipClass(grade: string): string {
  switch (grade.toUpperCase()) {
    case "A":
      return "grade-a";
    case "B":
      return "grade-b";
    case "C":
      return "grade-c";
    case "D":
      return "grade-d";
    case "F":
      return "grade-f";
    default:
      return "grade-unknown";
  }
}

function kindLabel(k: string): string {
  return k.replace(/_/g, " ").toUpperCase();
}

function fmtTime(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

function fmtMs(ms: number): string {
  if (ms <= 0) return "0ms";
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${(ms / 60000).toFixed(1)}m`;
}
