import { useState } from "react";
import type { SqlConnectionConfig } from "../store/persist";
import * as backend from "../api/backend";
import type { Recommendation, RecommendationKind, RecommendationPriority } from "../api/backend";

/**
 * The ADVISOR workspace. Connection is configured at SERVER scope (CONN
 * workspace); here we ask the backend for ranked, prescriptive fixes derived
 * from accumulated DMV signals (missing-index, unused-index, scan patterns).
 *
 * Each recommendation carries exact, copy-paste T-SQL — rendered in a <pre>
 * with a Copy button. The list is already ordered high→low server-side, so we
 * render it as-is.
 */
export function AdvisorPanel({ conn }: { conn: SqlConnectionConfig }) {
  const [recs, setRecs] = useState<Recommendation[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function analyze() {
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
      const { recommendations } = await backend.advise(info);
      setRecs(recommendations);
    } catch (e: any) {
      setErr(e?.message ?? String(e));
      setRecs(null);
    } finally {
      setBusy(false);
    }
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
        <div className="form-actions">
          <button className="btn primary" onClick={analyze} disabled={busy}>
            {busy ? "Analyzing…" : "Analyze server"}
          </button>
          {busy && <span className="advisor-spinner" aria-hidden />}
        </div>
        {busy && <div className="form-status">Scanning DMVs…</div>}
        {err && <div className="form-status err">{err}</div>}
      </div>

      {recs != null && !err && (
        recs.length === 0 ? (
          <div className="form-status">
            No recommendations right now. They appear after you Analyze the server. If
            still empty, the workload has not generated actionable DMV signals yet (no
            missing-index scans or unused indexes detected).
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

  return (
    <div className="advisor-card">
      <div className="advisor-card-head">
        <span className={`pill ${priorityClass(rec.priority)}`}>{rec.priority}</span>
        <span className="advisor-kind">{kindLabel(rec.kind)}</span>
        <span className="advisor-title">{rec.title}</span>
        <span className="advisor-score" title="impact score">
          {Math.round(rec.impact_score).toLocaleString()}
        </span>
      </div>
      <div className="advisor-object">
        <code>{rec.object}</code>
      </div>
      <div className="advisor-rationale">{rec.rationale}</div>
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
