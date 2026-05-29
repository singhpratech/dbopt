import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import type { SqlConnectionConfig, UiPrefs } from "../store/persist";
import * as backend from "../api/backend";
import type { Confidence, HealthReport, Issue, IssueSeverity } from "../api/backend";
import { IssueDetailPane } from "./IssueDetailPane";
import { MetricChip } from "./MetricChip";
import { Term } from "./Term";
import { CONF_GLYPH, CONF_LABEL, confGlyph, confTitle } from "../confidence";
import * as scanlog from "../store/scanlog";
import type { ScanDiff, ScanSnapshot } from "../store/scanlog";
import * as fixlog from "../store/fixlog";

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
  // Persistent re-scan trust loop: the prior PERSISTED snapshot (survives reload
  // / sessions) lets us compute an ISSUE-LEVEL diff — which fixes landed, what's
  // new, how the grades moved. Shown as a dismissible "Since last scan" strip
  // that stays until the user dismisses it (no 6s auto-dismiss).
  const [diff, setDiff] = useState<ScanDiff | null>(null);
  const [diffDismissed, setDiffDismissed] = useState(false);

  // Live view of the durable scan-history for THIS server·db (re-renders on
  // append/clear). Drives the "Scan history" trend table.
  const scanHistory = useSyncExternalStore(
    scanlog.subscribe,
    () => scanlog.history(conn.server, conn.database || undefined),
  );

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
      // Capture the prior persisted snapshot BEFORE we append this scan, so the
      // diff compares against the last run (not against itself).
      const prev = scanlog.latest(conn.server, conn.database || undefined);
      const r = await backend.getDbHealth(info);
      if (prev) {
        setDiff(scanlog.diff(prev, r));
        setDiffDismissed(false);
      } else {
        // First-ever scan for this server·db: nothing to diff against.
        setDiff(null);
      }
      // Persist this scan to the durable audit trail (server·db keyed).
      scanlog.append(conn.server, conn.database || undefined, r);
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
    setSelectedIssueId(null); // conn changed → close any open pane.
    // A new server/db is a new baseline — drop any visible diff so the strip
    // never compares across different databases (the diff is re-derived from
    // the persisted history of the new server·db on its next scan).
    setDiff(null);
    setDiffDismissed(false);
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
  // In learning mode the letters are not trustworthy yet — render them as
  // PROVISIONAL (see GradeBlock) instead of a confident full-contrast grade.
  const provisional = live && report!.is_learning === true;
  // Does the sentinel window actually hold runtime data? If we're learning we
  // treat runtime signals as "not monitored yet" (no honest 0 to show).
  const hasRuntimeData = live && !report!.is_learning;

  // The selected Issue is DERIVED from the id, never stored separately.
  const selectedIssue = report?.issues.find((i) => i.id === selectedIssueId) ?? null;
  // Toggle: re-clicking the same card closes the pane.
  const openIssue = useCallback(
    (id: string) => setSelectedIssueId((prev) => (prev === id ? null : id)),
    [],
  );

  // A2: post-fix verify loop. The "Next: Re-scan to verify →" breadcrumb on
  // issue cards + the detail pane re-runs the HEALTH scan (read-only, NO DDL) so
  // the user can prove the fix landed without leaving. Navigate to HEALTH first
  // (a no-op when already here) so the breadcrumb works from any context.
  const verifyRescan = useCallback(() => {
    if (ui.workspace !== "health") setUi({ ...ui, workspace: "health" });
    void scan();
  }, [ui, setUi, scan]);

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
            provisional={provisional}
          />
          <GradeBlock
            label="Efficiency"
            term="efficiency_grade"
            sublabel="Speed & cost to reclaim"
            grade={efficiencyGrade}
            score={live ? report!.efficiency_score : null}
            provisional={provisional}
          />
        </div>
        <div className="health-head-row2">
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
      </div>

      {/* Persistent re-scan trust loop: after a re-scan, an ISSUE-LEVEL "did the
          fix work?" strip — resolved/new titles + grade moves. Stays until the
          user dismisses it (survives reload via the scanlog store). */}
      {diff && !diffDismissed && !busy && (
        <SinceLastScan diff={diff} onDismiss={() => setDiffDismissed(true)} />
      )}

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
        <LearningBanner earliestAt={scanlog.earliestAt(conn.server, conn.database || undefined)} />
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
          {/* ── 2) SIGNAL STRIP ─────────────────────────────
              Structural signals (missing/unused/dup/columnstore) are always
              DMV-measured → shown as-is. Runtime signals (deadlocks/blocking/
              top wait/regressions) depend on accumulated sentinel history: when
              we have it, a subtle "measured" tick; when learning / no data, they
              read "— not monitored yet" instead of a falsely-reassuring 0. */}
          <div className="health-signals">
            <Signal label="missing idx" term="missing_index" value={report.signals.missing_indexes} measured />
            <Signal label="unused idx" term="unused_index" value={report.signals.unused_indexes} measured />
            <Signal label="duplicate idx" term="duplicate_index" value={report.signals.duplicate_indexes} measured />
            <Signal label="columnstore" term="columnstore" value={report.signals.columnstore_candidates} measured />
            <Signal
              label="deadlocks"
              term="deadlock"
              value={report.signals.deadlock_count}
              tone="crit"
              monitored={hasRuntimeData}
            />
            <Signal
              label="blocking"
              term="blocking"
              value={report.signals.blocking_incidents}
              tone="warn"
              monitored={hasRuntimeData}
            />
            <Signal
              label="top wait"
              term="wait_type"
              value={
                report.signals.top_wait_type
                  ? `${report.signals.top_wait_type} · ${fmtMs(report.signals.top_wait_time_ms)}`
                  : "—"
              }
              monitored={hasRuntimeData}
            />
            <Signal
              label="regressions"
              term="regression"
              value={report.signals.regressed_queries}
              tone="warn"
              monitored={hasRuntimeData}
            />
          </div>

          {/* ── 3) START HERE — the 1-3 issues to fix first ─ */}
          <StartHere issues={topIssues(report.issues)} onOpen={openIssue} />

          {/* ── 4) LANED ISSUE SECTIONS ─────────────────── */}
          <IssueSection
            tone="reliability"
            heading="RELIABILITY — affecting users"
            emptyLine="No reliability issues — users are unaffected."
            issues={report.issues.filter((i) => i.lane === "reliability")}
            conn={conn}
            ui={ui}
            setUi={setUi}
            onOpen={openIssue}
            onVerify={verifyRescan}
            verifying={busy}
          />
          <IssueSection
            tone="opportunity"
            heading="OPPORTUNITIES — performance & cost wins"
            emptyLine="Fully optimized — no opportunities found."
            issues={report.issues.filter((i) => i.lane === "opportunity")}
            conn={conn}
            ui={ui}
            setUi={setUi}
            onOpen={openIssue}
            onVerify={verifyRescan}
            verifying={busy}
          />

          {/* ── 5) SCAN HISTORY — durable trend / proof fixes moved the needle ── */}
          <ScanHistory
            entries={scanHistory}
            onClear={() => {
              scanlog.clear(conn.server, conn.database || undefined);
              setDiff(null);
            }}
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
              onVerifyRescan={verifyRescan}
              verifying={busy}
            />
          )}
        </>
      ) : null}
    </div>
  );
}

/** The baseline-monitoring window (days) after which learning grades firm up. */
const BASELINE_DAYS = 7;

/**
 * B3: the learning-mode banner with a concrete PROGRESS BAR + "grades firm up in
 * ~N more days", computed from the EARLIEST scanlog snapshot timestamp for this
 * server·db (the first time we ever saw it). Until we have that anchor we can't
 * date the baseline — fall back to the prose-only ETA. Honest: progress is from
 * when sqlopt first scanned, the proxy we actually have for "how long we've been
 * watching this database".
 */
function LearningBanner({ earliestAt }: { earliestAt: string | null }) {
  const days = daysSince(earliestAt);
  // Progress toward the ~7-day baseline, clamped to [0,1].
  const pct = days != null ? Math.max(0, Math.min(1, days / BASELINE_DAYS)) : null;
  const remaining = days != null ? Math.max(0, Math.ceil(BASELINE_DAYS - days)) : null;

  return (
    <div className="health-learning">
      Learning mode — DMV signal counters look freshly reset (post-restart). Absence of
      signal is not proof of health; the grade is provisional until a workload accumulates.
      {pct != null ? (
        <div className="health-learning-progress">
          <div
            className="health-learning-bar"
            role="progressbar"
            aria-valuemin={0}
            aria-valuemax={BASELINE_DAYS}
            aria-valuenow={Math.round((days ?? 0) * 10) / 10}
            aria-label="Baseline monitoring progress"
          >
            <span className="health-learning-bar-fill" style={{ width: `${pct * 100}%` }} />
          </div>
          <span className="health-learning-eta">
            {remaining && remaining > 0
              ? `Baseline building — grades firm up in ~${remaining} more ${
                  remaining === 1 ? "day" : "days"
                } of monitoring.`
              : "Baseline window reached — grades will firm up on the next scans as a workload accumulates."}
          </span>
        </div>
      ) : (
        <span className="health-learning-eta">
          Baseline builds as the server runs; grades firm up after ~{BASELINE_DAYS} days of monitoring.
        </span>
      )}
    </div>
  );
}

/**
 * One headline grade cell: big grade chip + score, with a plain-English sublabel.
 *
 * In `provisional` (learning) mode the LETTER itself must not read as a confident
 * grade — we gray it out and label it PROVISIONAL (with a tooltip), because the
 * dismissible learning banner alone is too easy to miss. We always show score
 * CONTEXT next to the grade ("C · 71/100") so the number is interpretable.
 */
function GradeBlock({
  label,
  term,
  sublabel,
  grade,
  score,
  provisional,
}: {
  label: string;
  /** Glossary slug — wraps the label so hovering explains the grade. */
  term: string;
  sublabel: string;
  grade: string;
  score: number | null;
  /** Learning mode → the grade is not trustworthy yet; show it as provisional. */
  provisional?: boolean;
}) {
  // When provisional we drop the band color (gray-out) so an A/F doesn't read as
  // confident; the chip carries the band only when the grade is real.
  const gradeClass = provisional ? "grade-provisional" : gradeChipClass(grade);
  const provisionalTip =
    "Provisional — baseline builds as the server runs; grades firm up after ~7 days of monitoring. (DMV stats reset on restart, so we don't penalize the grade yet.)";
  return (
    <div
      className={`health-grade${provisional ? " provisional" : ""}`}
      title={provisional ? provisionalTip : `${label} grade`}
    >
      <div className={`health-grade-chip ${gradeClass}`}>
        {provisional ? (
          <span
            className="pill grade-provisional health-grade-prov"
            title={provisionalTip}
          >
            {grade !== "?" ? grade : "—"}
            <span className="health-grade-prov-tag">PROVISIONAL</span>
          </span>
        ) : (
          <span className={`pill ${gradeClass}`}>{grade}</span>
        )}
        {/* Score context: "71/100" so the number is interpretable, not bare. */}
        <span className="health-score">
          {score != null ? (
            <>
              {score}
              <span className="health-score-denom">/100</span>
            </>
          ) : (
            "—"
          )}
        </span>
      </div>
      <div className="health-grade-meta">
        <div className="health-grade-label">
          <Term k={term}>{label}</Term>
        </div>
        <div className="health-grade-sub">
          {provisional ? (
            <Term k="learning_mode">provisional · still learning</Term>
          ) : (
            sublabel
          )}
        </div>
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
  conn,
  ui,
  setUi,
  onOpen,
  onVerify,
  verifying,
}: {
  tone: "reliability" | "opportunity";
  heading: string;
  emptyLine: string;
  issues: Issue[];
  /** Active connection — server·db scopes the per-issue fixlog (validated badge). */
  conn: SqlConnectionConfig;
  ui: UiPrefs;
  setUi: (u: UiPrefs) => void;
  onOpen: (id: string) => void;
  /** A2: trigger the HEALTH re-scan to verify a fix landed (read-only). */
  onVerify: () => void;
  /** True while a re-scan is in flight — disables the verify breadcrumb. */
  verifying: boolean;
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
            <IssueCard
              key={iss.id}
              iss={iss}
              conn={conn}
              ui={ui}
              setUi={setUi}
              onOpen={onOpen}
              onVerify={onVerify}
              verifying={verifying}
            />
          ))}
        </div>
      )}
    </section>
  );
}

/**
 * B2: the ONE per-card confidence indicator. Collapses the former N per-chip
 * tier glyphs into a single head-level badge — observed / estimated / heuristic
 * — with the SAME glanceable glyph vocabulary (✓ / ○ / ⚡) and a <Term> tooltip.
 * The per-metric source popover still lives on each chip; this is the single
 * trust signal for the card as a whole.
 */
function CardConfidence({ confidence }: { confidence?: Confidence }) {
  const c = confidence ?? "observed";
  return (
    <Term k="confidence" className={`confidence-badge card-confidence conf-${c}`}>
      <span className="confidence-badge-glyph" aria-hidden>
        {CONF_GLYPH[c]}
      </span>
      {CONF_LABEL[c]}
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
 * "Since last scan" strip — the persistent trust loop. Beyond the aggregate
 * grade move it names the ISSUE-LEVEL diff: which issues were RESOLVED (the
 * proof a fix landed), which are NEW, and how each grade shifted. Stays put
 * until dismissed; survives reload because it's derived from the durable
 * scanlog snapshot, not session memory.
 */
function SinceLastScan({ diff, onDismiss }: { diff: ScanDiff; onDismiss: () => void }) {
  const { resolved, added, reliability, efficiency } = diff;
  // The "headline" tone: net improvement (more resolved than new) reads up.
  const net = resolved.length - added.length;
  const tone = net > 0 ? "up" : net < 0 ? "down" : "flat";
  // A2: the most prominent score delta across the two axes (drives the headline
  // chip). Honest framing — this is the realized move "since last scan", not a
  // fabricated live latency. Pick the axis that moved the most (abs delta).
  const relD = reliability.toScore - reliability.fromScore;
  const effD = efficiency.toScore - efficiency.fromScore;
  const lead = Math.abs(effD) >= Math.abs(relD)
    ? { label: "Efficiency", d: effD, leg: efficiency }
    : { label: "Reliability", d: relD, leg: reliability };
  return (
    <div className={`since-scan tone-${tone}`} role="status">
      <div className="since-scan-row">
        <span className="since-scan-tag">Since last scan</span>
        <span className="since-scan-when">{relTime(diff.prevAt)}</span>
        {/* A2: prominent score delta — the realized move, framed honestly. */}
        <span
          className={`since-scan-delta dir-${lead.d > 0 ? "up" : lead.d < 0 ? "down" : "flat"}`}
          title={`${lead.label} moved ${lead.leg.fromScore} → ${lead.leg.toScore} since last scan`}
        >
          <span className="since-scan-delta-axis">{lead.label}</span>
          <span className="since-scan-delta-num">
            {lead.d > 0 ? "+" : ""}
            {lead.d}
          </span>
          <span className="since-scan-delta-grade">
            {lead.leg.fromGrade}
            <span className="since-scan-arrow" aria-hidden>→</span>
            {lead.leg.toGrade}
          </span>
        </span>
        <button className="since-scan-x" onClick={onDismiss} title="Dismiss" aria-label="Dismiss">
          ✕
        </button>
      </div>
      <div className="since-scan-body">
        {/* Issue-level — the headline, with each resolved issue's realized win. */}
        <IssueDiffLeg kind="resolved" issues={resolved} />
        <span className="since-scan-sep" aria-hidden>·</span>
        <IssueDiffLeg kind="added" issues={added} />
        <span className="since-scan-sep" aria-hidden>·</span>
        {/* Grade moves — the aggregate context. B3: carry the provisional tier
            forward so a move between two learning scans reads "provisional". */}
        <GradeLeg
          label="Reliability"
          leg={reliability}
          fromLearning={diff.fromLearning}
          toLearning={diff.toLearning}
        />
        <GradeLeg
          label="Efficiency"
          leg={efficiency}
          fromLearning={diff.fromLearning}
          toLearning={diff.toLearning}
        />
      </div>

      {/* A2: per-resolved realized wins — the headline metric each fix banked,
          pulled from that issue's PRIOR-snapshot metrics (the live report no
          longer carries it). Honest "resolved since last scan", not live latency. */}
      {resolved.length > 0 && (
        <ul className="since-scan-wins">
          {resolved.slice(0, 4).map((iss) => {
            const win = scanlog.realizedWin(iss);
            return (
              <li key={iss.id} className="since-scan-win">
                <span className="since-scan-win-check" aria-hidden>✓</span>
                <span className="since-scan-win-title">{iss.title}</span>
                {win && (
                  <span className="since-scan-win-metric" title="Realized since last scan">
                    {win}
                  </span>
                )}
                <span className="since-scan-win-tag">resolved since last scan</span>
              </li>
            );
          })}
          {resolved.length > 4 && (
            <li className="since-scan-win since-scan-win-more">
              +{resolved.length - 4} more resolved
            </li>
          )}
        </ul>
      )}
    </div>
  );
}

/** One side of the issue-level diff (resolved or added), naming up to 3 titles. */
function IssueDiffLeg({
  kind,
  issues,
}: {
  kind: "resolved" | "added";
  issues: { id: string; title: string }[];
}) {
  const word = kind === "resolved" ? "Resolved" : "New";
  const titles = issues.slice(0, 3).map((i) => i.title);
  const more = issues.length - titles.length;
  return (
    <span className={`since-scan-leg since-scan-${kind}`}>
      <span className="since-scan-leg-n">
        {word}: {issues.length}
      </span>
      {titles.length > 0 && (
        <span className="since-scan-leg-titles" title={issues.map((i) => i.title).join(" · ")}>
          {" "}
          {titles.join(" · ")}
          {more > 0 ? ` +${more} more` : ""}
        </span>
      )}
    </span>
  );
}

function GradeLeg({
  label,
  leg,
  fromLearning,
  toLearning,
}: {
  label: string;
  leg: ScanDiff["reliability"];
  /** B3: prior scan was in learning mode → its grade was provisional. */
  fromLearning?: boolean;
  /** B3: current scan is in learning mode → its grade is provisional. */
  toLearning?: boolean;
}) {
  const dir = deltaDir(leg.fromScore, leg.toScore);
  const gradeMoved = leg.fromGrade !== leg.toGrade;
  // B3: when learning, the letter isn't trustworthy yet — render it as
  // "provisional" so a move between two learning scans reads "provisional →
  // provisional" instead of implying a firm grade change.
  const fromLabel = fromLearning ? "provisional" : leg.fromGrade;
  const toLabel = toLearning ? "provisional" : leg.toGrade;
  const anyProvisional = fromLearning || toLearning;
  return (
    <span className={`since-scan-grade dir-${dir}${anyProvisional ? " provisional" : ""}`}>
      <span className="since-scan-grade-label">{label}</span>{" "}
      <span
        className="since-scan-grade-val"
        title={anyProvisional ? "Grade is provisional while the server is still in learning mode" : undefined}
      >
        {fromLabel}
        <span className="since-scan-arrow" aria-hidden>→</span>
        {toLabel}
      </span>{" "}
      <span className="since-scan-grade-detail">
        {gradeMoved
          ? `(${leg.toScore > leg.fromScore ? "+" : ""}${leg.toScore - leg.fromScore}, ${leg.fromScore}→${leg.toScore})`
          : dir === "flat"
          ? "(no change)"
          : `(${leg.fromScore}→${leg.toScore})`}
      </span>
    </span>
  );
}

/**
 * SCAN HISTORY — a collapsible, compact trend table of past scans (newest
 * first): time · Reliability grade · Efficiency grade · #issues. The durable
 * proof that fixes moved the needle across sessions. Clearable.
 */
function ScanHistory({
  entries,
  onClear,
}: {
  entries: ScanSnapshot[];
  onClear: () => void;
}) {
  // Default collapsed once there's enough trail to be interesting; if it's just
  // the single current scan there's no trend to see, so stay collapsed/quiet.
  const [open, setOpen] = useState(false);
  if (entries.length === 0) return null;
  return (
    <section className="scan-history">
      <div className="scan-history-head">
        <button
          className="scan-history-toggle"
          onClick={() => setOpen((o) => !o)}
          aria-expanded={open}
        >
          <span className="scan-history-caret" aria-hidden>
            {open ? "▾" : "▸"}
          </span>
          Scan history
          <span className="scan-history-count">{entries.length}</span>
        </button>
        {open && entries.length > 0 && (
          <button className="scan-history-clear" onClick={onClear} title="Forget the scan trail for this database">
            Clear
          </button>
        )}
      </div>
      {open && (
        <table className="scan-history-table">
          <thead>
            <tr>
              <th>Time</th>
              <th>Rel</th>
              <th>Eff</th>
              <th className="num">Issues</th>
            </tr>
          </thead>
          <tbody>
            {entries.map((e, i) => (
              <tr key={e.at} className={i === 0 ? "is-latest" : undefined}>
                <td className="scan-history-time" title={fmtTime(e.at)}>
                  {relTime(e.at)}
                  {i === 0 && <span className="scan-history-latest-tag">latest</span>}
                </td>
                <td>
                  <span className={`pill scan-history-grade ${gradeChipClass(e.reliability_grade)}`}>
                    {e.reliability_grade}
                  </span>
                </td>
                <td>
                  <span className={`pill scan-history-grade ${gradeChipClass(e.efficiency_grade)}`}>
                    {e.efficiency_grade}
                  </span>
                </td>
                <td className="num scan-history-issues">{e.issues.length}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

function Signal({
  label,
  term,
  value,
  tone,
  measured,
  monitored,
}: {
  label: string;
  /** Glossary slug — wraps the counter label so hovering explains it. */
  term?: string;
  value: number | string;
  tone?: "crit" | "warn";
  /** Structural signals are always DMV-measured → show a subtle measured tick. */
  measured?: boolean;
  /**
   * Runtime signals only: true when sentinel history exists (→ measured tick),
   * false when learning / no data (→ "— not monitored yet", a muted unknown
   * instead of a falsely-reassuring 0). Undefined = not a runtime signal.
   */
  monitored?: boolean;
}) {
  // A runtime signal with no sentinel history yet: don't imply healthy-zero.
  // B2: an unmistakable amber treatment + icon (not a muted gray "–") so this
  // reads as "unknown, action needed", not "fine".
  if (monitored === false) {
    return (
      <div
        className="health-signal not-monitored"
        title="No sentinel history yet — start WATCH (continuous monitoring) to track this"
      >
        <span className="health-signal-k">
          {term ? <Term k={term}>{label}</Term> : label}
        </span>
        <span className="health-signal-v unknown">
          <span className="health-signal-eye" aria-hidden>
            ◎
          </span>
          <span className="health-signal-note">not monitored yet</span>
        </span>
      </div>
    );
  }
  const isMeasured = measured || monitored === true;
  const hot = typeof value === "number" && value > 0 && tone;
  return (
    <div className="health-signal">
      <span className="health-signal-k">
        {term ? <Term k={term}>{label}</Term> : label}
        {isMeasured && (
          <span
            className="health-signal-measured conf-observed"
            title={confTitle("observed")}
            aria-label="observed"
          >
            {confGlyph("observed")}
          </span>
        )}
      </span>
      <span className={`health-signal-v${hot ? ` ${tone}` : ""}`}>
        {typeof value === "number" ? value.toLocaleString() : value}
      </span>
    </div>
  );
}

function IssueCard({
  iss,
  conn,
  ui,
  setUi,
  onOpen,
  onVerify,
  verifying,
}: {
  iss: Issue;
  /** Active connection — server·db scopes the per-issue fixlog (validated badge). */
  conn: SqlConnectionConfig;
  ui: UiPrefs;
  setUi: (u: UiPrefs) => void;
  onOpen: (id: string) => void;
  /** A2: trigger a HEALTH re-scan to verify a fix landed (read-only, no DDL). */
  onVerify: () => void;
  verifying: boolean;
}) {
  const [copied, setCopied] = useState(false);
  // B2: ONE disclosure now gates the lower-density detail (consequence +
  // rationale remainder + DDL) so the resting card is just title · chips ·
  // rationale lead. Reduces the 5-6 stacked sections to a tight 3.
  const [showDetails, setShowDetails] = useState(false);

  // B1: surface the user's manual "Validated ✓ (date)" assertion for this issue
  // (tracked in IssueDetailPane, persisted per server·db·issue). Live so the
  // badge appears the moment the user flips the toggle in the pane.
  const fix = useSyncExternalStore(
    fixlog.subscribe,
    () => fixlog.get(conn.server, conn.database || undefined, iss.id),
  );

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
  const { lead, rest } = iss.rationale ? splitRationale(iss.rationale) : { lead: "", rest: "" };
  // Is there anything worth a "details" disclosure? (consequence, more
  // rationale, or copy-paste DDL). If not, we drop the toggle entirely.
  const hasDetails = !!iss.consequence || !!rest || !!iss.fix_sql;
  const sevGutter = severityClass(iss.severity);

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
      className={`advisor-card health-issue health-issue-clickable health-issue-gutter gutter-${sevGutter}`}
      role="button"
      tabIndex={0}
      onClick={() => onOpen(iss.id)}
      onKeyDown={onKeyDown}
      aria-label={`View fix for ${iss.title}`}
    >
      <div className="advisor-card-head">
        <span className={`pill ${sevGutter}`}>{iss.severity}</span>
        <span className="advisor-kind">{kindLabel(iss.kind)}</span>
        <span className="advisor-title">{iss.title}</span>
        {/* B2: ONE per-card confidence indicator in the head (replaces the
            per-chip glyph density below). The per-chip source popover survives
            on the chips themselves. */}
        {iss.confidence && <CardConfidence confidence={iss.confidence} />}
        {/* B1: user-asserted "Validated ✓ (date)" badge once they mark it. */}
        {fix.validated && (
          <span
            className="health-validated-badge"
            title={
              fix.validatedAt
                ? `You marked this fix validated on ${fmtTime(fix.validatedAt)}`
                : "You marked this fix validated"
            }
          >
            ✓ Validated
            {fix.validatedAt && (
              <span className="health-validated-when"> · {fmtDateShort(fix.validatedAt)}</span>
            )}
          </span>
        )}
        <span className="advisor-score" title="impact rank">
          {iss.impact_rank.toLocaleString()}
        </span>
      </div>
      <div className="advisor-object">
        <code>{iss.affected_object}</code>
      </div>

      {/* Evidence: first 2-3 grounded metric chips. B2: the per-chip tier glyph
          is suppressed (hideGlyph) — the ONE per-card confidence indicator lives
          in the head — but each chip keeps its source popover on click/hover. */}
      {iss.metrics?.length > 0 && (
        <div className="metric-row">
          {(iss.metrics ?? []).slice(0, 3).map((m, i) => (
            <MetricChip key={i} metric={m} confidence={iss.confidence} hideGlyph />
          ))}
        </div>
      )}

      {/* A3: heuristic-caveat parity — the SAME "⚡ Heuristic — verify/benchmark
          before applying" note AdvisorPanel shows, on the card's View-fix path. */}
      {iss.confidence === "heuristic" && <HeuristicNote />}

      {/* The inline rationale lead — the only prose at rest. */}
      {lead && <p className="health-rationale-lead">{lead}</p>}

      {/* B2: everything else (impact, rest of rationale, DDL) collapses behind a
          SINGLE toggle so the resting card stays compact. */}
      {hasDetails && (
        <>
          <button
            className="health-toggle"
            aria-expanded={showDetails}
            onClick={(e) => {
              e.stopPropagation();
              setShowDetails((o) => !o);
            }}
          >
            {showDetails ? "▾ less" : "▸ details"}
          </button>
          {showDetails && (
            <div className="health-issue-details">
              {iss.consequence && <p className="health-consequence">{iss.consequence}</p>}
              {rest && <div className="advisor-rationale">{rest}</div>}
              {iss.fix_sql && (
                <div className="ddl-wrap" onClick={(e) => e.stopPropagation()}>
                  <button className="ddl-copy" onClick={copy} title="Copy fix SQL to clipboard">
                    {copied ? "Copied ✓" : "Copy"}
                  </button>
                  <pre className="ddl">{iss.fix_sql}</pre>
                </div>
              )}
            </div>
          )}
        </>
      )}

      {/* ONE prominent primary action (View fix → opens the detail pane); the
          deep-links are lighter secondary buttons. */}
      <div className="health-issue-foot">
        <button
          className="btn primary health-issue-primary"
          onClick={(e) => {
            e.stopPropagation();
            onOpen(iss.id);
          }}
        >
          View fix →
        </button>
        {links.map((l) => (
          <button
            key={l.workspace}
            className="health-issue-secondary"
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

      {/* A2: persistent post-fix verify breadcrumb — after copying the fix DDL,
          re-scan (read-only) to prove it landed. Always present so the loop is
          obvious; sqlopt never runs the DDL itself. */}
      <div className="verify-breadcrumb" onClick={(e) => e.stopPropagation()}>
        <span className="verify-breadcrumb-lead">Ran the fix in your SQL client?</span>
        <button
          className="verify-breadcrumb-btn"
          onClick={(e) => {
            e.stopPropagation();
            onVerify();
          }}
          disabled={verifying}
          title="Re-run the read-only HEALTH scan to confirm this issue is resolved"
        >
          {verifying ? "Re-scanning…" : "Next: Re-scan to verify →"}
        </button>
      </div>
    </div>
  );
}

/**
 * A3: the shared heuristic caveat — IDENTICAL wording + glyph to AdvisorPanel's
 * `.advisor-heuristic-note`, so the ⚡ glyph carries the same meaning everywhere
 * (columnstore recs are rule-of-thumb; verify/benchmark before applying).
 */
function HeuristicNote() {
  return (
    <p className="advisor-heuristic-note">
      <span className="advisor-heuristic-glyph" aria-hidden>
        {CONF_GLYPH.heuristic}
      </span>
      Heuristic — based on rule-of-thumb ratios, not a measured outcome. Benchmark a
      representative query before applying.
    </p>
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

/**
 * Split a rationale into a visible lead (first 1-2 sentences) and the collapsed
 * remainder. Sentence-aware: splits on ". " boundaries, keeping up to two
 * sentences inline. Returns `rest: ""` when there's nothing more to hide.
 */
function splitRationale(text: string): { lead: string; rest: string } {
  const trimmed = text.trim();
  // Split into sentences, preserving their terminators.
  const sentences = trimmed.match(/[^.!?]+[.!?]+(\s|$)|[^.!?]+$/g);
  if (!sentences || sentences.length <= 2) return { lead: trimmed, rest: "" };
  const lead = sentences.slice(0, 2).join("").trim();
  const rest = sentences.slice(2).join("").trim();
  return { lead, rest };
}

function fmtTime(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

/** Compact local date ("5/29/2026") for the inline "Validated ✓ · <date>" badge. */
function fmtDateShort(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleDateString();
}

/**
 * Fractional days elapsed since an ISO timestamp (null on missing/invalid), used
 * by the learning-mode progress bar to date the baseline from the earliest scan.
 */
function daysSince(iso: string | null): number | null {
  if (!iso) return null;
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return null;
  return Math.max(0, (Date.now() - t) / 86_400_000);
}

/**
 * Compact relative time ("2m ago", "3h ago", "yesterday") for the scan trail;
 * falls back to a local date for anything older than a week. Keeps the history
 * table scannable instead of full timestamps (those live in the title= hover).
 */
function relTime(iso: string): string {
  const d = new Date(iso).getTime();
  if (Number.isNaN(d)) return iso;
  const sec = Math.round((Date.now() - d) / 1000);
  if (sec < 5) return "just now";
  if (sec < 60) return `${sec}s ago`;
  const min = Math.round(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.round(min / 60);
  if (hr < 24) return `${hr}h ago`;
  const day = Math.round(hr / 24);
  if (day === 1) return "yesterday";
  if (day < 7) return `${day}d ago`;
  return new Date(d).toLocaleDateString();
}

function fmtMs(ms: number): string {
  if (ms <= 0) return "0ms";
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${(ms / 60000).toFixed(1)}m`;
}
