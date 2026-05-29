import { useCallback, useEffect, useRef, useState } from "react";
import type { SqlConnectionConfig, UiPrefs } from "../store/persist";
import * as backend from "../api/backend";
import type { Confidence, HealthReport, Issue, IssueSeverity, Metric } from "../api/backend";
import { IssueDetailPane } from "./IssueDetailPane";
import { Term } from "./Term";

/** A computed re-scan delta — how the two grades moved between scans. */
interface ScanDelta {
  reliability: { fromGrade: string; toGrade: string; fromScore: number; toScore: number };
  efficiency: { fromGrade: string; toGrade: string; fromScore: number; toScore: number };
}

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
  onOpenHelp,
}: {
  conn: SqlConnectionConfig;
  ui: UiPrefs;
  setUi: (u: UiPrefs) => void;
  /** Open the Help & glossary slide-over (optionally scrolled to a term). */
  onOpenHelp?: (focusTerm?: string) => void;
}) {
  const [report, setReport] = useState<HealthReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // The clicked issue, by id. Derived (not stored) into selectedIssue below —
  // collision-safe because Issue.id is unique post-dedup.
  const [selectedIssueId, setSelectedIssueId] = useState<string | null>(null);
  // Re-scan trust loop: the previous report (kept across a refresh) lets us
  // compute a "did the fix work?" delta banner; it auto-dismisses after ~6s.
  const prevReportRef = useRef<HealthReport | null>(null);
  const [delta, setDelta] = useState<ScanDelta | null>(null);
  const deltaTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const connected = !!conn.server;

  const scan = useCallback(async () => {
    if (!conn.server) return;
    setBusy(true);
    setErr(null);
    // A fresh scan re-keys the issue list; clear the open pane so a stale id
    // can't dangle against a re-built report.
    setSelectedIssueId(null);
    try {
      const info = {
        server: conn.server,
        database: conn.database || undefined,
        user: conn.auth_mode === "sql" ? conn.user : undefined,
        password: conn.auth_mode === "sql" ? conn.password : undefined,
        trust_cert: conn.trust_cert,
      };
      const r = await backend.getDbHealth(info);
      // Trust loop: if we already had a report THIS session, show how the two
      // grades moved (the proof a fix landed), then auto-dismiss after ~6s.
      const prev = prevReportRef.current;
      if (prev) {
        setDelta(computeDelta(prev, r));
        if (deltaTimer.current) clearTimeout(deltaTimer.current);
        deltaTimer.current = setTimeout(() => setDelta(null), 6000);
      }
      prevReportRef.current = r;
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

  // Clear the delta timer on unmount so it can't fire into an unmounted tree.
  useEffect(
    () => () => {
      if (deltaTimer.current) clearTimeout(deltaTimer.current);
    },
    [],
  );

  // Auto-fetch on mount + whenever the active server/database changes.
  useEffect(() => {
    setSelectedIssueId(null); // conn changed → close any open pane.
    // A new server/db is a new baseline — drop the prior report + any delta so
    // the trust banner never compares across different databases.
    prevReportRef.current = null;
    setDelta(null);
    if (deltaTimer.current) clearTimeout(deltaTimer.current);
    if (conn.server) void scan();
    else {
      setReport(null);
      setErr(null);
    }
  }, [conn.server, conn.database, scan]);

  // Each grade collapses to "?" when we have nothing real to show.
  const live = connected && !!report && !err;
  const reliabilityGrade = live ? report!.reliability_grade ?? "?" : "?";
  const efficiencyGrade = live ? report!.efficiency_grade ?? "?" : "?";

  // The selected Issue is DERIVED from the id, never stored separately.
  const selectedIssue = report?.issues.find((i) => i.id === selectedIssueId) ?? null;
  // Toggle: re-clicking the same card closes the pane.
  const openIssue = useCallback(
    (id: string) => setSelectedIssueId((prev) => (prev === id ? null : id)),
    [],
  );

  return (
    <div className="advisor form">
      {/* ── 1) DUAL-GRADE HEADER ──────────────────────── */}
      <div className={`health-header${busy ? " scanning" : ""}`}>
        <div className="health-grades">
          <GradeBlock
            label="Reliability"
            term="reliability_grade"
            sublabel="Are users hitting errors?"
            grade={reliabilityGrade}
            score={live ? report!.reliability_score : null}
          />
          <GradeBlock
            label="Efficiency"
            term="efficiency_grade"
            sublabel="Speed & cost to reclaim"
            grade={efficiencyGrade}
            score={live ? report!.efficiency_score : null}
          />
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
              : "one-screen snapshot + what to fix first"}
          </div>
        </div>
        <div className="form-actions health-actions">
          <button
            className="btn primary"
            onClick={() => void scan()}
            disabled={busy || !connected}
            title={report ? "Re-scan and prove the fix worked" : "Run the first scan"}
          >
            {busy ? "Scanning…" : report ? "Re-scan" : "Scan"}
          </button>
          {busy && <span className="advisor-spinner" aria-hidden />}
        </div>
      </div>

      {/* Re-scan trust loop: after a re-scan, show how the two grades moved.
          Auto-dismisses after ~6s — the "did the fix work?" proof. */}
      {delta && !busy && <RescanDelta delta={delta} onDismiss={() => setDelta(null)} />}

      {/* Plain-language read of the two grades, so the letters never stand alone. */}
      <p className="health-grade-explain">
        Two grades, two questions. <strong>Reliability</strong> asks{" "}
        <em>“are users hitting errors right now?”</em> (deadlocks, blocking, harmful waits).{" "}
        <strong>Efficiency</strong> asks <em>“how much speed and cost could you reclaim?”</em> —
        a lower efficiency grade means more easy wins are available, not that anything is broken.
        {onOpenHelp && (
          <>
            {" "}
            <button className="link-inline" onClick={() => onOpenHelp("reliability_grade")}>
              How grades work →
            </button>
          </>
        )}
      </p>

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
            <div className="empty-title">No SQL Server connected</div>
            <div className="empty-hint">
              Point sqlopt at a SQL Server instance and it will read the built-in
              performance views, then grade your database in plain English. Nothing
              leaves your machine.
            </div>
            <div className="empty-action">
              <button className="btn primary" onClick={() => setUi({ ...ui, workspace: "connection" })}>
                Connect a SQL Server
              </button>
            </div>
            {onOpenHelp && (
              <div className="empty-sub-action">
                New here?{" "}
                <button className="link-inline" onClick={() => onOpenHelp()}>
                  Open the guide
                </button>
              </div>
            )}
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
            <Signal label="missing idx" term="missing_index" value={report.signals.missing_indexes} />
            <Signal label="unused idx" term="unused_index" value={report.signals.unused_indexes} />
            <Signal label="duplicate idx" term="duplicate_index" value={report.signals.duplicate_indexes} />
            <Signal label="columnstore" term="columnstore" value={report.signals.columnstore_candidates} />
            <Signal label="deadlocks" term="deadlock" value={report.signals.deadlock_count} tone="crit" />
            <Signal label="blocking" term="blocking" value={report.signals.blocking_incidents} tone="warn" />
            <Signal
              label="top wait"
              term="wait_type"
              value={
                report.signals.top_wait_type
                  ? `${report.signals.top_wait_type} · ${fmtMs(report.signals.top_wait_time_ms)}`
                  : "—"
              }
            />
            <Signal label="regressions" term="regression" value={report.signals.regressed_queries} tone="warn" />
          </div>

          {/* ── 3) START HERE — the 1-3 issues to fix first ─ */}
          <StartHere issues={topIssues(report.issues)} onOpen={openIssue} />

          {/* ── 4) LANED ISSUE SECTIONS ─────────────────── */}
          <IssueSection
            tone="reliability"
            heading="RELIABILITY — affecting users"
            emptyLine="No reliability issues — users are unaffected."
            issues={report.issues.filter((i) => i.lane === "reliability")}
            ui={ui}
            setUi={setUi}
            onOpen={openIssue}
          />
          <IssueSection
            tone="opportunity"
            heading="OPPORTUNITIES — performance & cost wins"
            emptyLine="Fully optimized — no opportunities found."
            issues={report.issues.filter((i) => i.lane === "opportunity")}
            ui={ui}
            setUi={setUi}
            onOpen={openIssue}
          />

          {/* ── Issue Detail slide-over — sibling of the list so the
                health context stays mounted underneath. ──────────── */}
          {selectedIssue && (
            <IssueDetailPane
              key={selectedIssue.id}
              issue={selectedIssue}
              conn={conn}
              ui={ui}
              setUi={setUi}
              onClose={() => setSelectedIssueId(null)}
            />
          )}
        </>
      ) : null}
    </div>
  );
}

/** One headline grade cell: big grade chip + score, with a plain-English sublabel. */
function GradeBlock({
  label,
  term,
  sublabel,
  grade,
  score,
}: {
  label: string;
  /** Glossary slug — wraps the label so hovering explains the grade. */
  term: string;
  sublabel: string;
  grade: string;
  score: number | null;
}) {
  const gradeClass = gradeChipClass(grade);
  return (
    <div className="health-grade" title={`${label} grade`}>
      <div className={`health-grade-chip ${gradeClass}`}>
        <span className={`pill ${gradeClass}`}>{grade}</span>
        <span className="health-score">{score != null ? score : "—"}</span>
      </div>
      <div className="health-grade-meta">
        <div className="health-grade-label">
          <Term k={term}>{label}</Term>
        </div>
        <div className="health-grade-sub">{sublabel}</div>
      </div>
    </div>
  );
}

/** A laned group of issue cards with a counted header and an empty-line fallback. */
function IssueSection({
  tone,
  heading,
  emptyLine,
  issues,
  ui,
  setUi,
  onOpen,
}: {
  tone: "reliability" | "opportunity";
  heading: string;
  emptyLine: string;
  issues: Issue[];
  ui: UiPrefs;
  setUi: (u: UiPrefs) => void;
  onOpen: (id: string) => void;
}) {
  return (
    <section className={`health-section health-section-${tone}`}>
      <div className="section-header">
        <span className="section-dot" aria-hidden>
          ●
        </span>
        <span className="section-title">{heading}</span>
        <span className="section-count">{issues.length}</span>
      </div>
      {issues.length === 0 ? (
        <div className="section-empty">{emptyLine}</div>
      ) : (
        <div className="health-issue-list">
          {issues.map((iss) => (
            <IssueCard key={iss.id} iss={iss} ui={ui} setUi={setUi} onOpen={onOpen} />
          ))}
        </div>
      )}
    </section>
  );
}

/**
 * One evidence chip — a grounded label/value pair (e.g. "Writes maintained
 * 412/wk"). Pre-formatted server-side; rendered verbatim. Reuses .pill geometry.
 */
function MetricChip({ metric }: { metric: Metric }) {
  return (
    <span className="metric-chip" title={`${metric.label}: ${metric.value}`}>
      <span className="metric-chip-k">{metric.label}</span>
      <span className="metric-chip-v">{metric.value}</span>
    </span>
  );
}

/**
 * Small provenance badge — observed / estimated / heuristic — with a <Term>
 * tooltip explaining the difference so we never imply fake precision.
 */
function ConfidenceBadge({ confidence }: { confidence?: Confidence }) {
  const c = confidence ?? "observed";
  return (
    <Term k="confidence" className={`confidence-badge conf-${c}`}>
      {c}
    </Term>
  );
}

/**
 * START HERE — the focus callout. Names the top 1-3 issues to fix first as
 * clickable chips that open their detail pane. Highest-severity reliability
 * wins over opportunity; ties break on impact_rank.
 */
function StartHere({ issues, onOpen }: { issues: Issue[]; onOpen: (id: string) => void }) {
  if (issues.length === 0) return null;
  return (
    <div className="start-here" role="group" aria-label="Where to start">
      <span className="start-here-tag">START HERE</span>
      <span className="start-here-lead">
        Fix {issues.length === 1 ? "this first" : `these ${issues.length} first`}:
      </span>
      <div className="start-here-chips">
        {issues.map((iss) => (
          <button
            key={iss.id}
            type="button"
            className={`start-here-chip ${iss.lane === "reliability" ? "lane-rel" : "lane-opp"}`}
            onClick={() => onOpen(iss.id)}
            title={iss.consequence || iss.title}
          >
            <span className={`start-here-dot ${severityClass(iss.severity)}`} aria-hidden>
              ●
            </span>
            <span className="start-here-chip-title">{iss.title}</span>
            <span className="start-here-chip-go" aria-hidden>
              →
            </span>
          </button>
        ))}
      </div>
    </div>
  );
}

/**
 * Re-scan delta banner — the trust loop. Shows how each grade moved between the
 * previous and current scan (e.g. "Reliability A→A · Efficiency C→B (71→81)").
 */
function RescanDelta({ delta, onDismiss }: { delta: ScanDelta; onDismiss: () => void }) {
  return (
    <div className="rescan-delta" role="status">
      <span className="rescan-delta-tag">Re-scan</span>
      <DeltaLeg label="Reliability" leg={delta.reliability} />
      <span className="rescan-delta-sep" aria-hidden>
        ·
      </span>
      <DeltaLeg label="Efficiency" leg={delta.efficiency} />
      <button className="rescan-delta-x" onClick={onDismiss} title="Dismiss" aria-label="Dismiss">
        ✕
      </button>
    </div>
  );
}

function DeltaLeg({
  label,
  leg,
}: {
  label: string;
  leg: ScanDelta["reliability"];
}) {
  const dir = deltaDir(leg.fromScore, leg.toScore);
  const gradeMoved = leg.fromGrade !== leg.toGrade;
  return (
    <span className={`rescan-delta-leg dir-${dir}`}>
      <span className="rescan-delta-label">{label}</span>{" "}
      <span className="rescan-delta-grades">
        {leg.fromGrade}
        <span className="rescan-delta-arrow" aria-hidden>
          →
        </span>
        {leg.toGrade}
      </span>{" "}
      <span className="rescan-delta-detail">
        {gradeMoved
          ? `(${leg.toScore > leg.fromScore ? "+" : ""}${leg.toScore - leg.fromScore} score, ${leg.fromScore}→${leg.toScore})`
          : dir === "flat"
          ? "(no change)"
          : `(${leg.fromScore}→${leg.toScore})`}
      </span>
    </span>
  );
}

function Signal({
  label,
  term,
  value,
  tone,
}: {
  label: string;
  /** Glossary slug — wraps the counter label so hovering explains it. */
  term?: string;
  value: number | string;
  tone?: "crit" | "warn";
}) {
  const hot = typeof value === "number" && value > 0 && tone;
  return (
    <div className="health-signal">
      <span className="health-signal-k">
        {term ? <Term k={term}>{label}</Term> : label}
      </span>
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
  onOpen,
}: {
  iss: Issue;
  ui: UiPrefs;
  setUi: (u: UiPrefs) => void;
  onOpen: (id: string) => void;
}) {
  const [copied, setCopied] = useState(false);
  const [open, setOpen] = useState(false);

  async function copy(e: React.MouseEvent) {
    e.stopPropagation(); // don't also open the detail pane
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

  // The whole card is a button into the detail pane; inner controls stop
  // propagation so they keep their own behavior without also opening the pane.
  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onOpen(iss.id);
    }
  }

  return (
    <div
      className="advisor-card health-issue health-issue-clickable"
      role="button"
      tabIndex={0}
      onClick={() => onOpen(iss.id)}
      onKeyDown={onKeyDown}
      aria-label={`View fix for ${iss.title}`}
    >
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

      {/* Evidence: first 2-3 grounded metric chips + a provenance badge. */}
      {(iss.metrics?.length > 0 || iss.confidence) && (
        <div className="metric-row">
          {(iss.metrics ?? []).slice(0, 3).map((m, i) => (
            <MetricChip key={i} metric={m} />
          ))}
          <ConfidenceBadge confidence={iss.confidence} />
        </div>
      )}

      {iss.consequence && <p className="health-consequence">{iss.consequence}</p>}

      {iss.rationale && (
        <>
          <button
            className="health-toggle"
            onClick={(e) => {
              e.stopPropagation();
              setOpen((o) => !o);
            }}
          >
            {open ? "▾ rationale" : "▸ rationale"}
          </button>
          {open && <div className="advisor-rationale">{iss.rationale}</div>}
        </>
      )}

      <div className="health-issue-foot">
        <span className="health-issue-cta" aria-hidden>
          View fix →
        </span>
        {links.map((l) => (
          <button
            key={l.workspace}
            className="ddl-copy"
            onClick={(e) => {
              e.stopPropagation();
              setUi({ ...ui, workspace: l.workspace });
            }}
            title={`Jump to the ${l.label} workspace`}
          >
            {l.label}
          </button>
        ))}
      </div>

      {iss.fix_sql && (
        <div className="ddl-wrap" onClick={(e) => e.stopPropagation()}>
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

/**
 * Pick the top 1-3 issues to fix first. Reliability outranks opportunity (active
 * harm beats a cheaper win); within a lane, higher severity then higher
 * impact_rank wins. The backend already pre-ranks, so this is a light re-sort.
 */
const SEVERITY_ORDER: Record<IssueSeverity, number> = {
  critical: 0,
  error: 1,
  warning: 2,
  info: 3,
};
function topIssues(issues: Issue[]): Issue[] {
  return [...issues]
    .sort((a, b) => {
      // Reliability first.
      if (a.lane !== b.lane) return a.lane === "reliability" ? -1 : 1;
      // Then by severity (critical first).
      const sev = SEVERITY_ORDER[a.severity] - SEVERITY_ORDER[b.severity];
      if (sev !== 0) return sev;
      // Then by impact_rank (higher = fix first).
      return b.impact_rank - a.impact_rank;
    })
    .slice(0, 3);
}

/** Build the two-grade delta between a previous and a current scan. */
function computeDelta(prev: HealthReport, next: HealthReport): ScanDelta {
  return {
    reliability: {
      fromGrade: prev.reliability_grade ?? "?",
      toGrade: next.reliability_grade ?? "?",
      fromScore: prev.reliability_score,
      toScore: next.reliability_score,
    },
    efficiency: {
      fromGrade: prev.efficiency_grade ?? "?",
      toGrade: next.efficiency_grade ?? "?",
      fromScore: prev.efficiency_score,
      toScore: next.efficiency_score,
    },
  };
}

/** up = score improved, down = regressed, flat = unchanged (drives leg color). */
function deltaDir(from: number, to: number): "up" | "down" | "flat" {
  if (to > from) return "up";
  if (to < from) return "down";
  return "flat";
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
