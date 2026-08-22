import { useCallback, useEffect, useState } from "react";
import type { SqlConnectionConfig, UiPrefs } from "../store/persist";
import * as backend from "../api/backend";
import { CounterAgeChip } from "./CounterAgeChip";
import type { Recommendation, RecommendationKind, RecommendationPriority } from "../api/backend";
import { Term, TermText } from "./Term";
import { CONF_GLYPH, confTier } from "../confidence";

/**
 * The ADVISOR workspace — the ranked, full-detail view of the fixes summarised
 * on HEALTH. Connection is configured at SERVER scope (CONN workspace); here we
 * ask the backend for ranked, prescriptive fixes derived from accumulated DMV
 * signals (missing-index, unused-index, scan patterns).
 *
 * It AUTO-RUNS on entry whenever a connection exists (mirroring HealthOverview's
 * auto-fetch) so the workspace lands populated rather than as a hollow button +
 * empty state; a manual "Re-analyze" stays for an explicit refresh. With no
 * connection it shows the same friendly connect CTA HEALTH uses.
 *
 * Each recommendation carries exact, copy-paste T-SQL — rendered in a <pre>
 * with a Copy button. The list is already ordered high→low server-side, so we
 * render it as-is.
 */
export function AdvisorPanel({
  conn,
  ui,
  setUi,
}: {
  conn: SqlConnectionConfig;
  ui: UiPrefs;
  setUi: (u: UiPrefs) => void;
}) {
  const [recs, setRecs] = useState<Recommendation[] | null>(null);
  // Age of the DMV usage counters behind these verdicts (absent on older backends).
  const [counterAge, setCounterAge] = useState<{ secs?: number | null; since?: string | null }>({});
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  // Connected enough to scan = a server AND a database (advise is DB-scoped).
  const connected = !!conn.server;
  const ready = connected && !!conn.database;

  const analyze = useCallback(async () => {
    if (!conn.server) {
      setErr("Configure a SQL Server connection first (CONN workspace).");
      return;
    }
    if (!conn.database) {
      setErr("Pick a database first (CONN workspace).");
      return;
    }
    setBusy(true);
    setErr(null);
    try {
      const info = {
        server: conn.server,
        database: conn.database,
        user: conn.auth_mode === "sql" ? conn.user : undefined,
        password: conn.auth_mode === "sql" ? conn.password : undefined,
        trust_cert: conn.trust_cert,
      };
      const res = await backend.advise(info);
      setRecs(res.recommendations);
      setCounterAge({ secs: res.counter_age_secs, since: res.counters_since });
    } catch (e: any) {
      setErr(backend.humanizeError(e));
      setRecs(null);
    } finally {
      setBusy(false);
    }
    // conn identity fields are the only inputs; deliberately keyed below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conn.server, conn.database, conn.user, conn.password, conn.auth_mode, conn.trust_cert]);

  // Auto-run on mount + whenever the active server/database changes, so ADVISE
  // is never a hollow empty button when HEALTH points users here.
  useEffect(() => {
    if (ready) void analyze();
    else {
      setRecs(null);
      setErr(null);
    }
  }, [ready, analyze]);

  // Not connected → the same friendly connect CTA pattern HEALTH uses, never a
  // bare empty card.
  if (!connected) {
    return (
      <div className="advisor form">
        <div className="empty">
          <div className="empty-card">
            <div className="empty-glyph">✦</div>
            <div className="empty-title">No SQL Server connected</div>
            <div className="empty-hint">
              The advisor reads accumulated DMV signals (missing-index scans, unused and
              duplicate indexes, columnstore candidates) and ranks exact, copy-paste fixes.
              Connect a SQL Server to populate it — the analysis runs here, and your schema never leaves this machine.
            </div>
            <div className="empty-action">
              <button className="btn primary" onClick={() => setUi({ ...ui, workspace: "connection" })}>
                Connect a SQL Server
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="advisor form">
      <div className="form-section">
        <h4>
          Prescriptive advisor{" "}
          <span style={{ color: "var(--text-dim)", fontWeight: 400, fontSize: 11 }}>
            · ranked fixes from accumulated DMV signals
            {conn.database ? <> · {conn.database}</> : null}
          </span>
        </h4>
        {/* Clarify the relationship to HEALTH so the two views don't feel like
            disconnected silos. */}
        <p className="advisor-header-note">
          The ranked, full-detail view of the fixes summarised on HEALTH.
        </p>
        <div className="form-actions">
          <button className="btn primary" onClick={() => void analyze()} disabled={busy || !ready}>
            {busy ? "Analyzing…" : recs != null ? "Re-analyze" : "Analyze server"}
          </button>
          {busy && <span className="advisor-spinner" aria-hidden />}
        </div>
        {busy && <div className="form-status">Scanning DMVs…</div>}
        {/* Connected to a server but no DB chosen — advise is DB-scoped. */}
        {!ready && !busy && !err && (
          <div className="form-status">
            Pick a database in the{" "}
            <button className="link-inline" onClick={() => setUi({ ...ui, workspace: "connection" })}>
              CONN
            </button>{" "}
            workspace to run the advisor.
          </div>
        )}
        {err && <div className="form-status err">{err}</div>}
        {recs != null && !err && (
          <div className="counter-age-row">
            <CounterAgeChip ageSecs={counterAge.secs} since={counterAge.since} />
          </div>
        )}
      </div>

      {recs != null && !err && (
        recs.length === 0 ? (
          <div className="form-status">
            No recommendations right now — the workload has not generated actionable DMV
            signals yet (no missing-index scans, unused or duplicate indexes detected). Re-run
            after the database has served a representative workload, or after a SQL restart has
            re-accumulated usage stats.
          </div>
        ) : (
          <div className="advisor-list">
            {recs.map((r, i) => (
              <RecCard key={i} rec={r} />
            ))}
          </div>
        )
      )}
    </div>
  );
}

function RecCard({ rec }: { rec: Recommendation }) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(rec.ddl);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      /* clipboard unavailable (insecure context) — silently ignore */
    }
  }

  // A3: surface the confidence tier (glyph + label) so users see which recs are
  // grounded vs rule-of-thumb. Columnstore lands as "heuristic" (⚡) — the
  // strongest "verify first" signal — using the ONE shared vocabulary.
  const tier = confTier(rec.confidence);

  return (
    <div className="advisor-card">
      <div className="advisor-card-head">
        <span className={`pill ${priorityClass(rec.priority)}`}>{rec.priority}</span>
        <span className="advisor-kind">
          <Term k={kindTerm(rec.kind)}>{kindLabel(rec.kind)}</Term>
        </span>
        <Term k="confidence" className={`confidence-badge conf-${tier}`}>
          <span className="confidence-badge-glyph" aria-hidden>
            {CONF_GLYPH[tier]}
          </span>
          {tier}
        </Term>
        <span className="advisor-title">{rec.title}</span>
        <span className="advisor-score" title="impact score">
          {Math.round(rec.impact_score).toLocaleString()}
        </span>
      </div>
      <div className="advisor-object">
        <code>{rec.object}</code>
      </div>
      <div className="advisor-rationale"><TermText>{rec.rationale}</TermText></div>
      {tier === "heuristic" && (
        <p className="advisor-heuristic-note">
          <span className="advisor-heuristic-glyph" aria-hidden>
            {CONF_GLYPH.heuristic}
          </span>
          Heuristic — based on rule-of-thumb ratios, not a measured outcome. Benchmark a
          representative query before applying.
        </p>
      )}
      <div className="ddl-wrap">
        <button className="ddl-copy" onClick={copy} title="Copy T-SQL to clipboard">
          {copied ? "Copied ✓" : "Copy"}
        </button>
        <pre className="ddl">{rec.ddl}</pre>
      </div>
    </div>
  );
}

function priorityClass(p: RecommendationPriority): string {
  switch (p) {
    case "high":   return "crit";
    case "medium": return "warn";
    case "low":    return "dim";
  }
}

function kindLabel(k: RecommendationKind): string {
  switch (k) {
    case "create_index":         return "CREATE INDEX";
    case "drop_index":           return "DROP INDEX";
    case "merge_index":          return "MERGE INDEX";
    case "columnstore_candidate": return "COLUMNSTORE";
    default:                     return k;
  }
}

/** Map a rec kind onto a glossary slug for the hover definition on its chip. */
function kindTerm(k: RecommendationKind): string {
  switch (k) {
    case "create_index":          return "missing_index";
    case "drop_index":            return "unused_index";
    case "merge_index":           return "duplicate_index";
    case "columnstore_candidate": return "columnstore";
    default:                      return "__none__";
  }
}
