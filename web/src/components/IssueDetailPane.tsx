import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { SqlConnectionConfig, UiPrefs } from "../store/persist";
import * as backend from "../api/backend";
import type {
  Confidence,
  Issue,
  IssueSeverity,
  Metric,
  Remediation,
  RemediationStep,
  RiskLevel,
  SolutionOption,
} from "../api/backend";
import { MetricChip } from "./MetricChip";
import { Term, TermText } from "./Term";
import { CONF_GLYPH } from "../confidence";
import * as fixlog from "../store/fixlog";

/**
 * Issue Detail + Remediation — a right-side SLIDE-OVER pane (not a route, not a
 * modal) that renders ONE structured Remediation uniformly. The health list
 * stays mounted underneath so context (grade/signals) is preserved.
 *
 * Two sourcing tiers, ONE shape:
 *  • investigate kinds (deadlock/blocking/wait/regression) → POST the issue to
 *    /api/health/issue/detail; the backend builds the Remediation from live
 *    sentinel data it already holds (parsed deadlock graph, blocking sample,
 *    wait table, regression row).
 *  • advisor kinds (missing/unused/duplicate/columnstore) + `finding` → built
 *    client-side from fields already on the Issue (fix_sql, rationale,
 *    affected_object). No network round-trip.
 *
 * Playbook principles folded in for v1: because-before-fix (diagnosis →
 * evidence → fix order), problem+fix in the same currency (impact line),
 * coarse severity backed by a number (impact_rank), show COST next to benefit
 * (per-step + ladder notes), honest confidence, and the explicit caveat that
 * copy-and-run means the USER owns the monitoring (no auto-validate net).
 *
 * Three dismiss gestures, all → onClose: × button, ESC, re-click the same card
 * (toggle wired in HealthOverview).
 */
export function IssueDetailPane({
  issue,
  conn,
  ui,
  setUi,
  onClose,
  onVerifyRescan,
  verifying,
}: {
  issue: Issue;
  conn: SqlConnectionConfig;
  ui: UiPrefs;
  setUi: (u: UiPrefs) => void;
  onClose: () => void;
  /** A2: re-run the read-only HEALTH scan to verify the fix landed (no DDL). */
  onVerifyRescan?: () => void;
  /** True while that re-scan is in flight — disables the verify breadcrumb. */
  verifying?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [rem, setRem] = useState<Remediation | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  // Trigger the slide-in transform on the frame AFTER mount so the transition
  // actually fires (mounting with .open already set would skip the animation).
  useEffect(() => {
    const r = requestAnimationFrame(() => setOpen(true));
    return () => cancelAnimationFrame(r);
  }, []);

  // ESC closes (the third dismiss gesture); cleaned up on unmount.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Load (or build) the Remediation whenever the selected issue changes.
  // investigate kinds hit the backend; everything else is templated locally.
  useEffect(() => {
    let cancelled = false;
    if (issue.fix_action === "investigate") {
      setBusy(true);
      setErr(null);
      setRem(null);
      const info = {
        server: conn.server,
        database: conn.database || undefined,
        user: conn.auth_mode === "sql" ? conn.user : undefined,
        password: conn.auth_mode === "sql" ? conn.password : undefined,
        trust_cert: conn.trust_cert,
      };
      backend
        .getIssueDetail(info, issue)
        .then((r) => {
          if (!cancelled) setRem(r);
        })
        .catch((e: unknown) => {
          if (!cancelled) setErr(e instanceof Error ? e.message : String(e));
        })
        .finally(() => {
          if (!cancelled) setBusy(false);
        });
    } else {
      setErr(null);
      setBusy(false);
      setRem(buildTemplateRemediation(issue));
    }
    return () => {
      cancelled = true;
    };
    // conn identity fields + the issue identity are the only inputs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    issue.id,
    issue.fix_action,
    conn.server,
    conn.database,
    conn.user,
    conn.password,
    conn.auth_mode,
    conn.trust_cert,
  ]);

  const links = deepLinks(issue);

  return (
    <aside
      className={`issue-detail-pane${open ? " open" : ""}`}
      role="dialog"
      aria-modal="false"
      aria-label={`Remediation for ${issue.title}`}
    >
      {/* ── Header / identity bar ──────────────────────── */}
      <div className="issue-detail-header">
        <div className="issue-detail-id">
          <span className={`pill ${severityClass(issue.severity)}`}>{issue.severity}</span>
          <span className="advisor-kind">{kindLabel(issue.kind)}</span>
          <span className="issue-detail-rank" title="impact rank — the number behind the severity">
            rank {issue.impact_rank.toLocaleString()}
          </span>
          <button className="ddl-copy issue-detail-close" onClick={onClose} title="Close (Esc)">
            ✕
          </button>
        </div>
        <h2 className="issue-detail-title">{issue.title}</h2>
        <AffectedObject object={issue.affected_object} />
        {issue.consequence && <p className="issue-detail-consequence">{issue.consequence}</p>}

        {/* Evidence: ALL grounded metric chips + a provenance badge. */}
        {((issue.metrics?.length ?? 0) > 0 || issue.confidence) && (
          <div className="metric-row issue-detail-metrics">
            {(issue.metrics ?? []).map((m, i) => (
              <MetricChip key={i} metric={m} confidence={issue.confidence} />
            ))}
            <ConfidenceBadge confidence={issue.confidence} />
          </div>
        )}

        {/* A3: heuristic-caveat parity — the SAME "⚡ Heuristic — verify/benchmark
            before applying" note AdvisorPanel uses, so the ⚡ glyph means the same
            thing on the View-fix path (columnstore candidates). */}
        {issue.confidence === "heuristic" && (
          <p className="advisor-heuristic-note">
            <span className="advisor-heuristic-glyph" aria-hidden>
              {CONF_GLYPH.heuristic}
            </span>
            Heuristic — based on rule-of-thumb ratios, not a measured outcome. Benchmark a
            representative query before applying.
          </p>
        )}
      </div>

      {/* ── Body ───────────────────────────────────────── */}
      <div className="issue-detail-body">
        {busy ? (
          <div className="issue-detail-loading">
            <span className="advisor-spinner" aria-hidden /> Building remediation from live sentinel
            data…
          </div>
        ) : err ? (
          <div className="form-status err issue-detail-err">{err}</div>
        ) : rem ? (
          <RemediationView rem={rem} issue={issue} conn={conn} />
        ) : null}
      </div>

      {/* A2: persistent post-fix verify breadcrumb — after copying the fix DDL,
          re-scan (read-only) to prove it landed. dbopt never runs the DDL. */}
      {onVerifyRescan && (
        <div className="verify-breadcrumb issue-detail-verify">
          <span className="verify-breadcrumb-lead">Ran the fix in your SQL client?</span>
          <button
            className="verify-breadcrumb-btn"
            onClick={onVerifyRescan}
            disabled={verifying}
            title="Re-run the read-only HEALTH scan to confirm this issue is resolved"
          >
            {verifying ? "Re-scanning…" : "Next: Re-scan to verify →"}
          </button>
        </div>
      )}

      {/* ── Action footer ──────────────────────────────── */}
      <div className="issue-detail-footer">
        {rem?.fix_sql && <CopyButton sql={rem.fix_sql} label="Copy fix SQL" />}
        {links.map((l) => (
          <button
            key={l.workspace}
            className="ddl-copy issue-detail-link"
            onClick={() => setUi({ ...ui, workspace: l.workspace })}
            title={`Jump to the ${l.label} workspace`}
          >
            {l.label}
          </button>
        ))}
      </div>
    </aside>
  );
}

/**
 * The body sections rendered in because-before-fix order.
 *
 * B1 threads `issue` + `conn` through so the Solution steps become an
 * EXECUTION-FREE interactive checklist (ticks persisted per server·db·issue in
 * the fixlog store), the Pre-flight PREVIEW can summarize the DDL + cost chips +
 * rollback, and a manual "Mark validated" toggle records the user's own "I ran
 * + verified this" assertion. None of this runs anything against the database.
 */
function RemediationView({
  rem,
  issue,
  conn,
}: {
  rem: Remediation;
  issue: Issue;
  conn: SqlConnectionConfig;
}) {
  const hasLadder = !!rem.solutions && rem.solutions.length > 0;
  const db = conn.database || undefined;

  // Live fixlog entry for this issue (re-renders on tick / validate / reset).
  const fix = useSyncExternalStore(
    fixlog.subscribe,
    () => fixlog.get(conn.server, db, issue.id),
  );

  return (
    <>
      <Section title="Diagnosis" tone="diagnosis">
        <p className="issue-detail-prose">
          <TermText>{rem.diagnosis}</TermText>
        </p>
      </Section>

      <Section title="Solution" tone="solution">
        {/* B1: solution steps as a PERSISTED checklist — the user ticks each off
            as they work it in their own SQL client. No execution; pure tracking. */}
        {rem.solution_steps.length > 0 && (
          <ol className="issue-steps issue-steps-checklist">
            {rem.solution_steps.map((s, i) => (
              <StepItem
                key={i}
                step={s}
                index={i}
                done={fix.stepsDone.includes(i)}
                onToggle={() => fixlog.toggleStep(conn.server, db, issue.id, i)}
                trackable={!!conn.server}
              />
            ))}
          </ol>
        )}

        {/* Ranked ladder (investigate kinds): each rung pairs benefit + cost. */}
        {hasLadder && (
          <div className="issue-ladder">
            <div className="issue-ladder-h">Ranked options — safest / most-likely first</div>
            {[...rem.solutions!]
              .sort((a, b) => a.rank - b.rank)
              .map((opt) => (
                <SolutionCard key={opt.rank} opt={opt} />
              ))}
          </div>
        )}

        {/* Primary executable DDL (advisor kinds). */}
        {rem.fix_sql && (
          <div className="ddl-wrap">
            <CopyButton sql={rem.fix_sql} label="Copy" />
            <pre className="ddl">{rem.fix_sql}</pre>
          </div>
        )}
      </Section>

      {/* B1: PRE-FLIGHT PREVIEW — what the fix DDL will do, its cost chips, and
          the rollback, with the explicit "you run this — dbopt does not execute"
          line. Purely informational; renders only when there's DDL to preview. */}
      {rem.fix_sql && <PreflightPreview rem={rem} issue={issue} />}

      <Section title="Apply safely" tone="apply">
        <Checklist items={rem.apply_safely} />
        {/* Honest about the safety boundary (playbook §7): copy-and-run means
            the user owns the monitoring; there is no auto-validate/revert net. */}
        <p className="issue-detail-caveat">
          You run these yourself. dbopt ships the script + validation steps — it does NOT auto-apply
          or auto-revert in v1, so you own monitoring the change.
        </p>
      </Section>

      {/* B1: MARK VALIDATED — a manual, persisted "I ran the fix + verified it"
          assertion. NOT an executed check; it just records the user's own state
          and surfaces a "Validated ✓ (date)" badge back on the HEALTH card. */}
      <Section title="Mark validated" tone="validated">
        <MarkValidated
          validated={fix.validated}
          validatedAt={fix.validatedAt}
          disabled={!conn.server}
          onToggle={(v) => fixlog.setValidated(conn.server, db, issue.id, v)}
        />
      </Section>

      <Section title="Validate" tone="validate">
        <Checklist items={rem.validate} ordered />
      </Section>

      <Section title="Rollback" tone="rollback">
        <Checklist items={rem.rollback} />
      </Section>

      <Section title="Impact & confidence" tone="impact">
        <p className="issue-detail-impact">
          <TermText>{rem.impact}</TermText>
        </p>
      </Section>

      {/* Deadlock graph — render the parsed supplemental as a readable cycle
          (A ──waits for──▶ B ──▶ A, victim marked) instead of raw JSON. The raw
          JSON stays available collapsed beneath for power users. */}
      {rem.supplemental != null && (
        <Section title="Deadlock graph" tone="raw">
          <DeadlockGraph supplemental={rem.supplemental} />
          <details className="issue-supplemental">
            <summary>Show parsed deadlock graph JSON</summary>
            <pre className="ddl issue-supplemental-pre">
              {JSON.stringify(rem.supplemental, null, 2)}
            </pre>
          </details>
        </Section>
      )}
    </>
  );
}

/* ============================================================
   Deadlock graph → readable cycle. The backend's parsed supplemental
   carries the victim + processes (each with SQL) + the resource
   owner→waiter chain. We render it as
       Session A (UPDATE orders…) ──waits for──▶ Session B (…) ──▶ A
   with the victim marked. Shape is best-effort / defensive: if the
   expected fields are absent we fall back to the raw-JSON details
   block only (the generic text path).
   ============================================================ */

interface DeadlockProcess {
  id?: string;
  spid?: string | number;
  session_id?: string | number;
  sql?: string;
  statement?: string;
  is_victim?: boolean;
}
interface DeadlockEdge {
  /** Resource owner (holds the lock). */
  owner?: string;
  /** Resource waiter (blocked on it). */
  waiter?: string;
  resource?: string;
}
interface DeadlockGraphShape {
  victim?: string;
  processes?: DeadlockProcess[];
  edges?: DeadlockEdge[];
  /** Some parsers emit the chain under "chain"/"waits"/"owner_waiter". */
  chain?: DeadlockEdge[];
  waits?: DeadlockEdge[];
  owner_waiter?: DeadlockEdge[];
}

function DeadlockGraph({ supplemental }: { supplemental: unknown }) {
  const g = (supplemental ?? {}) as DeadlockGraphShape;
  const procs = Array.isArray(g.processes) ? g.processes : [];
  const edges =
    (Array.isArray(g.edges) && g.edges) ||
    (Array.isArray(g.chain) && g.chain) ||
    (Array.isArray(g.waits) && g.waits) ||
    (Array.isArray(g.owner_waiter) && g.owner_waiter) ||
    [];

  // Victim: explicit field, else the process flagged is_victim.
  const victimId =
    (typeof g.victim === "string" && g.victim) ||
    procs.find((p) => p.is_victim)?.id ||
    (procs.find((p) => p.is_victim)?.session_id != null
      ? String(procs.find((p) => p.is_victim)?.session_id)
      : undefined);

  // If we can't recover a recognisable graph, signal the caller to fall back.
  if (procs.length === 0 && edges.length === 0) {
    return (
      <p className="issue-detail-prose dim">
        No structured graph was parsed for this deadlock — see the raw artifact below.
      </p>
    );
  }

  const procLabel = (id?: string | number) => procKey(procFind(procs, id));
  const procSqlOf = (id?: string | number) => {
    const p = procFind(procs, id);
    return p ? p.sql ?? p.statement : undefined;
  };

  // Build the visual cycle from edges if present, else just list participants.
  const nodes = edges.length > 0 ? edgeChainNodes(edges) : procs.map((p) => procKey(p));

  return (
    <div className="deadlock-cycle">
      {nodes.length > 1 ? (
        <div className="deadlock-chain">
          {nodes.map((nid, i) => {
            const isVictim = victimId != null && nid === victimId;
            const sql = procSqlOf(nid);
            return (
              <span className="deadlock-node-wrap" key={`${nid}-${i}`}>
                <span className={`deadlock-node${isVictim ? " victim" : ""}`}>
                  <span className="deadlock-node-id">
                    Session {procLabel(nid)}
                    {isVictim && <span className="deadlock-victim-tag"> · victim</span>}
                  </span>
                  {sql && <code className="deadlock-node-sql">{truncate(sql, 120)}</code>}
                </span>
                {i < nodes.length - 1 && (
                  <span className="deadlock-arrow" aria-label="waits for">
                    <span className="deadlock-arrow-label">waits for</span>
                    <span className="deadlock-arrow-glyph" aria-hidden>
                      ──▶
                    </span>
                  </span>
                )}
              </span>
            );
          })}
          {/* Close the cycle back to the first node (deadlocks are cyclic). */}
          {nodes.length > 1 && (
            <span className="deadlock-node-wrap deadlock-cycle-close">
              <span className="deadlock-arrow" aria-label="waits for">
                <span className="deadlock-arrow-label">waits for</span>
                <span className="deadlock-arrow-glyph" aria-hidden>
                  ──▶
                </span>
              </span>
              <span className="deadlock-node ghost">
                <span className="deadlock-node-id">Session {procLabel(nodes[0])}</span>
              </span>
            </span>
          )}
        </div>
      ) : (
        // Single recognizable participant — list it without a (meaningless) cycle.
        <div className="deadlock-chain">
          {procs.map((p, i) => {
            const id = p.id ?? (p.session_id != null ? String(p.session_id) : undefined);
            const isVictim = victimId != null && id === victimId;
            return (
              <span className="deadlock-node-wrap" key={i}>
                <span className={`deadlock-node${isVictim ? " victim" : ""}`}>
                  <span className="deadlock-node-id">
                    Session {procKey(p)}
                    {isVictim && <span className="deadlock-victim-tag"> · victim</span>}
                  </span>
                  {(p.sql ?? p.statement) && (
                    <code className="deadlock-node-sql">{truncate(p.sql ?? p.statement!, 120)}</code>
                  )}
                </span>
              </span>
            );
          })}
        </div>
      )}
      <p className="deadlock-cycle-note">
        SQL Server broke the cycle by killing the victim and rolling back its transaction. Re-run
        the loser, or apply a fix below so the cycle can't form again.
      </p>
    </div>
  );
}

/** Order the owner→waiter edges into a single chain of node ids. */
function edgeChainNodes(edges: DeadlockEdge[]): string[] {
  const out: string[] = [];
  for (const e of edges) {
    const owner = e.owner != null ? String(e.owner) : undefined;
    const waiter = e.waiter != null ? String(e.waiter) : undefined;
    if (owner && !out.includes(owner)) out.push(owner);
    if (waiter && !out.includes(waiter)) out.push(waiter);
  }
  return out;
}

function procFind(procs: DeadlockProcess[], id?: string | number): DeadlockProcess | undefined {
  if (id == null) return undefined;
  const s = String(id);
  return procs.find(
    (p) =>
      p.id === s ||
      (p.session_id != null && String(p.session_id) === s) ||
      (p.spid != null && String(p.spid) === s),
  );
}

/** Best display key for a process: its session id / spid / id, else the raw id. */
function procKey(p?: DeadlockProcess | string | number): string {
  if (p == null) return "?";
  if (typeof p === "string" || typeof p === "number") return String(p);
  return String(p.session_id ?? p.spid ?? p.id ?? "?");
}

function truncate(s: string, n: number): string {
  const t = s.trim();
  return t.length > n ? t.slice(0, n - 1) + "…" : t;
}

/**
 * Provenance badge with a <Term> tooltip — observed / estimated / heuristic.
 * The leading glyph (✓ / ○ / ⚡) is the SAME glanceable vocabulary used by the
 * metric chips and the HealthOverview signal strip.
 */
function ConfidenceBadge({ confidence }: { confidence?: Confidence }) {
  const c = confidence ?? "observed";
  return (
    <Term k="confidence" className={`confidence-badge conf-${c}`}>
      <span className="confidence-badge-glyph" aria-hidden>
        {CONF_GLYPH[c]}
      </span>
      {c}
    </Term>
  );
}

function Section({
  title,
  tone,
  children,
}: {
  title: string;
  tone: string;
  children: React.ReactNode;
}) {
  return (
    <section className={`issue-section issue-section-${tone}`}>
      <div className="issue-section-h">{title}</div>
      {children}
    </section>
  );
}

/**
 * One solution step. B1: now an EXECUTION-FREE checklist item — a checkbox the
 * user ticks as they work the step in their own SQL client. The done state is
 * persisted (fixlog) per server·db·issue. When `trackable` is false (no active
 * server) the checkbox is omitted — there's nothing to key the persistence to.
 */
function StepItem({
  step,
  index,
  done,
  onToggle,
  trackable,
}: {
  step: RemediationStep;
  index: number;
  done: boolean;
  onToggle: () => void;
  trackable: boolean;
}) {
  return (
    <li className={`issue-step${done ? " step-done" : ""}`}>
      <label className="issue-step-check">
        {trackable && (
          <input
            type="checkbox"
            className="issue-step-box"
            checked={done}
            onChange={onToggle}
            aria-label={`Mark step ${index + 1} done`}
          />
        )}
        <span className="issue-step-title">
          <TermText>{step.title}</TermText>
        </span>
      </label>
      {step.detail && (
        <span className="issue-step-detail">
          <TermText>{step.detail}</TermText>
        </span>
      )}
      {step.sql && (
        <div className="ddl-wrap">
          <CopyButton sql={step.sql} label="Copy" />
          <pre className="ddl">{step.sql}</pre>
        </div>
      )}
    </li>
  );
}

/**
 * B1: PRE-FLIGHT PREVIEW — a small, purely-informational section that previews
 * exactly what the fix DDL will do BEFORE the user runs it themselves. It shows:
 *   • the fix DDL (copyable),
 *   • cost chips derived from the issue's existing metrics (index size / tempdb /
 *     write overhead) — no new measurement, just the grounded chips we already
 *     hold, framed as the COST of applying,
 *   • the rollback (so the undo path is visible before committing), and
 *   • an explicit "Run this in your SQL client — dbopt does not execute changes"
 *     line so the execution boundary is unmistakable.
 * dbopt executes nothing here — this is a read-only preview.
 */
function PreflightPreview({ rem, issue }: { rem: Remediation; issue: Issue }) {
  const costs = costChips(issue);
  return (
    <Section title="Pre-flight — preview before you run it" tone="preflight">
      <p className="preflight-lead">
        This is exactly what the fix will do. dbopt does not run it — you apply it yourself.
      </p>

      {rem.fix_sql && (
        <div className="preflight-block">
          <div className="preflight-block-h">DDL to run</div>
          <div className="ddl-wrap">
            <CopyButton sql={rem.fix_sql} label="Copy" />
            <pre className="ddl">{rem.fix_sql}</pre>
          </div>
        </div>
      )}

      {costs.length > 0 && (
        <div className="preflight-block">
          <div className="preflight-block-h">Cost of applying</div>
          <div className="metric-row preflight-costs">
            {costs.map((m, i) => (
              <MetricChip key={i} metric={m} confidence={issue.confidence} />
            ))}
          </div>
        </div>
      )}

      {rem.rollback.length > 0 && (
        <div className="preflight-block">
          <div className="preflight-block-h">Rollback if needed</div>
          <Checklist items={rem.rollback} />
        </div>
      )}

      <p className="preflight-boundary">
        <span className="preflight-boundary-glyph" aria-hidden>
          ⌘
        </span>
        Run this in your SQL client (SSMS / sqlcmd) — <strong>dbopt does not execute changes.</strong>
      </p>
    </Section>
  );
}

/**
 * B1: MARK VALIDATED — the manual, execution-free "I ran + verified this fix"
 * toggle. Persisted per server·db·issue (fixlog). When ON it shows a
 * "Validated ✓ (date)" line (the same badge surfaces on the HEALTH issue card).
 * This asserts nothing about the database — it's the user's own bookkeeping.
 */
function MarkValidated({
  validated,
  validatedAt,
  disabled,
  onToggle,
}: {
  validated: boolean;
  validatedAt: string | null;
  disabled: boolean;
  onToggle: (v: boolean) => void;
}) {
  return (
    <div className="mark-validated">
      <label className={`mark-validated-toggle${validated ? " on" : ""}`}>
        <input
          type="checkbox"
          checked={validated}
          disabled={disabled}
          onChange={(e) => onToggle(e.target.checked)}
          aria-label="Mark this fix as validated"
        />
        <span className="mark-validated-label">
          {validated ? "Validated ✓" : "Mark validated"}
        </span>
      </label>
      {validated && validatedAt && (
        <span className="mark-validated-when">Validated {fmtDate(validatedAt)}</span>
      )}
      <p className="mark-validated-note">
        A manual record that you ran the fix in your SQL client and verified it — dbopt does not
        execute or check anything for you.
      </p>
    </div>
  );
}

/**
 * Derive the "cost of applying" chips from the issue's existing metrics — never
 * a new measurement. Prefers chips whose label signals a COST (size / tempdb /
 * write / overhead / maintenance); falls back to the first couple of grounded
 * metrics so the preview is never empty when evidence exists.
 */
function costChips(issue: Issue): Metric[] {
  const metrics = issue.metrics ?? [];
  if (metrics.length === 0) return [];
  const COST = /\b(size|kb|mb|gb|tb|tempdb|write|writes|overhead|maintenance|fragment|rebuild)\b/i;
  const costy = metrics.filter((m) => COST.test(m.label) || COST.test(m.value));
  return (costy.length > 0 ? costy : metrics).slice(0, 3);
}

/** Friendly absolute date for the "Validated ✓ (date)" line. */
function fmtDate(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleDateString();
}

/** One rung of the ranked ladder — benefit (estimated_impact) beside cost (notes). */
function SolutionCard({ opt }: { opt: SolutionOption }) {
  return (
    <div className="advisor-card issue-solution">
      <div className="advisor-card-head">
        <span className={`pill ${riskClass(opt.risk_level)}`}>{riskLabel(opt.risk_level)}</span>
        <span className="advisor-kind">{opt.category}</span>
        <span className="advisor-score" title="ladder rank">
          #{opt.rank}
        </span>
      </div>
      <p className="issue-detail-prose">
        <TermText>{opt.description}</TermText>
      </p>
      {opt.sql_fix && (
        <div className="ddl-wrap">
          <CopyButton sql={opt.sql_fix} label="Copy" />
          <pre className="ddl">{opt.sql_fix}</pre>
        </div>
      )}
      <dl className="issue-solution-meta">
        <div>
          <dt>Expected benefit</dt>
          <dd>{opt.estimated_impact}</dd>
        </div>
        {opt.notes && (
          <div>
            <dt>Cost / caveat</dt>
            <dd>{opt.notes}</dd>
          </div>
        )}
      </dl>
    </div>
  );
}

function Checklist({ items, ordered }: { items: string[]; ordered?: boolean }) {
  if (items.length === 0) return <p className="issue-detail-prose dim">None.</p>;
  const List = ordered ? "ol" : "ul";
  return (
    <List className={`issue-checklist${ordered ? " ordered" : ""}`}>
      {items.map((it, i) => (
        <li key={i}>
          <InlineText text={it} />
        </li>
      ))}
    </List>
  );
}

/**
 * Render a checklist line, splitting any embedded SQL onto its own copyable
 * block. Many apply/validate strings carry a "label: SELECT …" shape; surface
 * the query as code so the user can copy + drill straight to the statement.
 */
function InlineText({ text }: { text: string }) {
  const split = extractSql(text);
  if (!split) return <>{text}</>;
  const prose = text.slice(0, split.index).replace(/[:\-\s]+$/, "");
  return (
    <>
      {prose && <span>{prose}</span>}
      <div className="ddl-wrap issue-inline-sql">
        <CopyButton sql={split.sql} label="Copy" />
        <pre className="ddl">{split.sql}</pre>
      </div>
    </>
  );
}

/** Clipboard button with a textarea fallback for insecure (non-https) contexts. */
function CopyButton({ sql, label }: { sql: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const copy = useCallback(async () => {
    let ok = false;
    try {
      await navigator.clipboard.writeText(sql);
      ok = true;
    } catch {
      // clipboard API unavailable (insecure context) → execCommand fallback.
      try {
        const ta = document.createElement("textarea");
        ta.value = sql;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        document.body.appendChild(ta);
        ta.select();
        ok = document.execCommand("copy");
        document.body.removeChild(ta);
      } catch {
        ok = false;
      }
    }
    if (ok) {
      setCopied(true);
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => setCopied(false), 1400);
    }
  }, [sql]);

  useEffect(() => () => {
    if (timer.current) clearTimeout(timer.current);
  }, []);

  return (
    <button className="ddl-copy" onClick={copy} title="Copy to clipboard">
      {copied ? "Copied ✓" : label}
    </button>
  );
}

/** Affected object with a one-click copy (always a drill-path to the object). */
function AffectedObject({ object }: { object: string }) {
  const [copied, setCopied] = useState(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(object);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {
      /* insecure context — ignore */
    }
  }
  return (
    <div className="issue-detail-object">
      <code>{object}</code>
      <button className="issue-detail-object-copy" onClick={copy} title="Copy object name">
        {copied ? "copied" : "copy"}
      </button>
    </div>
  );
}

/* ============================================================
   Client-side template builder for the FRONTEND kinds.
   Produces the identical TS Remediation shape — IssueDetailPane
   renders it the same as a backend-built one. Templates mirror
   detail-spec.md §perKindSolutions 1-4 + 9, with the playbook's
   cost-beside-benefit + honest-confidence woven into impact.
   ============================================================ */
export function buildTemplateRemediation(iss: Issue): Remediation {
  const obj = iss.affected_object;
  const idx = extractIndexName(iss.fix_sql) ?? "the index";
  const base: Pick<Remediation, "issue_id" | "issue_kind"> = {
    issue_id: iss.id,
    issue_kind: iss.kind,
  };

  switch (iss.kind) {
    case "missing_index":
      return {
        ...base,
        diagnosis:
          (iss.rationale ? iss.rationale + " " : "") +
          `Estimated impact rank: ${iss.impact_rank.toLocaleString()}/10000. ` +
          `Query seeks on ${obj} are unsupported by an index and force scans.`,
        solution_steps: [
          {
            title: "Review the generated CREATE INDEX before running it.",
            detail:
              "Confirm the key + INCLUDE columns match your hottest predicates; do not blindly create wide indexes.",
          },
          {
            title: "Offline key order is by SARGable role, not measured selectivity.",
            detail:
              "Without a connection the analyzer orders key columns by role (equality predicates before range/inequality), not by measured histogram selectivity. Connect (or check statistics) to confirm the most selective equality column leads.",
          },
          {
            title: "Prefer ONLINE on Enterprise/Developer (2016 SP2+) to avoid blocking DML.",
            detail: "Add WITH (ONLINE = ON) to the DDL if your edition supports it.",
          },
          { title: "Run off-peak — index builds compete with the live workload." },
        ],
        fix_sql: iss.fix_sql,
        apply_safely: [
          "Back up or confirm a recent backup.",
          "Check active workload: SELECT session_id, blocking_session_id FROM sys.dm_exec_requests WHERE database_id = DB_ID()",
          "ONLINE = ON requires Enterprise/Developer; otherwise schedule a maintenance window.",
          "Verify no in-flight ALTER TABLE / partition switch on the table.",
          "Check existing indexes first to avoid a redundant/overlapping add (see ADVISE / INDEX).",
        ],
        validate: [
          `SELECT COUNT(*) FROM sys.indexes WHERE object_id = OBJECT_ID('${obj}') AND name = '${idx}' -- expect 1`,
          "Re-run the workload and confirm the new index is sought (Query Store / actual plan).",
          "Re-run the health scan; the missing_indexes signal should drop.",
        ],
        rollback: [`DROP INDEX [${idx}] ON ${obj}; -- metadata-only, instant`],
        impact:
          "Low-cost structural add (~index size in KB) but it adds write/maintenance overhead on every INSERT/UPDATE. Typically 5-50% faster on affected queries. Confidence: medium.",
      };

    case "unused_index":
      return {
        ...base,
        diagnosis:
          (iss.rationale ? iss.rationale + " " : "") +
          "Index accumulated writes but no reads in the window; pure maintenance overhead.",
        solution_steps: [
          {
            title: "Confirm zero reads over a LONGER window via the ADVISE / INDEX workspace.",
            detail:
              "A single snapshot can mislead — usage stats reset on SQL restart and monthly-report indexes look idle most days.",
          },
          {
            title: "Prefer DISABLE over DROP if replication is active or you are uncertain.",
          },
          { title: "Then DROP — but capture the CREATE first (DROP loses the definition)." },
        ],
        fix_sql: iss.fix_sql,
        apply_safely: [
          "Confirm the index has never served seeks/scans: check sys.dm_db_index_usage_stats last_user_seek / last_user_scan over usage HISTORY, not one read.",
          "If transactional/snapshot replication is active, DISABLE instead of DROP.",
          "Check for index hints / forced plans referencing it by name.",
          "Never drop a UNIQUE / PK-supporting index.",
        ],
        validate: [
          `SELECT COUNT(*) FROM sys.indexes WHERE object_id = OBJECT_ID('${obj}') AND name = '${idx}' -- expect 0`,
          "Run the workload 24-48h; verify no NEW missing-index rec re-appears for the same table.",
        ],
        rollback: [
          "Re-create from the original definition (capture the CREATE INDEX BEFORE dropping — DROP loses it).",
        ],
        impact:
          "Removes write-amplification + storage. 0.5-5% INSERT/UPDATE latency relief; storage reclaimed. Confidence: medium.",
      };

    case "duplicate_index":
      return {
        ...base,
        diagnosis:
          (iss.rationale ? iss.rationale + " " : "") +
          "Two+ indexes share key columns; redundant maintenance on every write.",
        solution_steps: [
          {
            title:
              "Confirm the surviving index covers the dropped one's key + included columns.",
            detail: "Watch for hidden columns (clustering key, UNIQUIFIER) before assuming overlap.",
          },
          { title: "Drop the redundant index." },
        ],
        fix_sql: iss.fix_sql,
        apply_safely: [
          "Verify no stored proc / view / indexed-view hard-references the index name.",
          "Confirm neither is a filtered / LOB index on a critical path.",
          "Test in dev if possible; back up.",
          "Never drop a UNIQUE / PK-supporting index.",
        ],
        validate: [
          `SELECT name FROM sys.indexes WHERE object_id = OBJECT_ID('${obj}') -- only the surviving index remains`,
          "Compare plans before/after to confirm the optimizer still seeks.",
        ],
        rollback: ["Re-create the dropped index from its original DDL."],
        impact:
          "Fewer page splits + stats updates per write; storage reclaimed. Confidence: medium.",
      };

    case "columnstore_candidate":
      return {
        ...base,
        diagnosis:
          (iss.rationale ? iss.rationale + " " : "") +
          "Wide, high-row-count table suited to column compression; currently row-store.",
        solution_steps: [
          { title: "Benchmark a representative aggregate query FIRST." },
          {
            title: "If OLTP-heavy, prefer a NONCLUSTERED columnstore over a clustered one.",
            detail: "Single-row seeks can regress under a clustered columnstore.",
          },
          { title: "Create off-peak — the build rebuilds the whole table." },
        ],
        fix_sql: iss.fix_sql,
        apply_safely: [
          "CCI creation is offline and rebuilds the whole table — plan a maintenance window (minutes to hours by size).",
          "No LOB/XML columns for a clustered columnstore.",
          "OLTP single-row seeks may regress slightly — confirm acceptable.",
          "Monitor tempdb (can spike 2-5x during the build).",
        ],
        validate: [
          `SELECT COUNT(*) FROM sys.indexes WHERE object_id = OBJECT_ID('${obj}') AND type_desc LIKE '%COLUMNSTORE%' -- expect 1`,
          "Run a typical SUM / GROUP BY; expect multi-x speedup.",
          "Check compression via sys.dm_db_index_physical_stats.",
        ],
        rollback: [
          `DROP INDEX [${idx}] ON ${obj}; then re-create the original clustered / row-store index (can be slow).`,
        ],
        impact:
          "70-90% storage reduction (5-15x), 3-10x faster scans/aggregates; minor single-row seek regression — that is the cost. Confidence: medium-low — TEST first.",
      };

    case "finding":
    default:
      return {
        ...base,
        diagnosis:
          (iss.rationale ? iss.rationale + " " : "") +
          `Rule: ${obj}.`,
        solution_steps: [
          {
            title: "Review the rule context.",
            detail:
              "Apply the recommended schema/config change only if it applies to your workload.",
          },
        ],
        fix_sql: iss.fix_sql,
        apply_safely: [
          "Advisory / design finding — generally low risk.",
          "Confirm it applies before changing schema.",
        ],
        validate: ["Re-run analysis; the finding should not re-appear if remediated."],
        rollback: ["Revert the schema / config change."],
        impact:
          "Improves schema health / maintainability; rarely an immediate perf win. Confidence: low.",
      };
  }
}

/* ── small pure helpers (local; mirror HealthOverview's where shared) ── */

/** Pull the index name out of a CREATE/DROP INDEX statement, if present. */
function extractIndexName(sql?: string): string | null {
  if (!sql) return null;
  // CREATE [UNIQUE] [CLUSTERED|NONCLUSTERED] INDEX [name] | DROP INDEX [name]
  const m =
    sql.match(/INDEX\s+\[?([A-Za-z0-9_#@$]+)\]?/i) ?? null;
  return m ? m[1] : null;
}

/** Heuristically split a trailing SQL statement off a "label: SELECT …" line. */
function extractSql(text: string): { sql: string; index: number } | null {
  const m = text.match(/\b(SELECT|ALTER|DROP|CREATE|UPDATE|EXEC|DBCC)\b[\s\S]+$/i);
  if (!m || m.index == null) return null;
  const sql = m[0].trim();
  return sql.length > 6 ? { sql, index: m.index } : null;
}

function deepLinks(iss: Issue): { workspace: UiPrefs["workspace"]; label: string }[] {
  switch (iss.kind) {
    case "missing_index":
    case "duplicate_index":
    case "columnstore_candidate":
      return [{ workspace: "advisor", label: "Review in ADVISE →" }];
    case "unused_index":
      return [
        { workspace: "advisor", label: "Review in ADVISE →" },
        { workspace: "indexes", label: "Open INDEX →" },
      ];
    case "regression":
      return [{ workspace: "history", label: "Open RUNS →" }];
    case "deadlock":
    case "blocking":
    case "wait":
      return [{ workspace: "sentinel", label: "Open SENTINEL →" }];
    default:
      return [];
  }
}

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

function riskClass(r: RiskLevel): string {
  switch (r) {
    case "safe":
      return "info";
    case "moderate":
      return "warn";
    case "risky":
      return "crit";
  }
}

function riskLabel(r: RiskLevel): string {
  return r;
}

function kindLabel(k: string): string {
  return k.replace(/_/g, " ").toUpperCase();
}
