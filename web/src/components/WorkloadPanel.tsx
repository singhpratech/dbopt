import { useCallback, useEffect, useState } from "react";
import type { SqlConnectionConfig } from "../store/persist";
import * as P from "../store/persist";
import * as backend from "../api/backend";
import type { SlowQuery } from "../api/backend";

// WORKLOAD — the slowest queries from the engine's captured query history,
// ranked by average duration. Read-only telemetry: we read the database's
// persisted query stats, we never execute the queries or read table rows.
// Deliberately uses our own vocabulary (no vendor feature names in the UI).

function ms(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)} s`;
  return `${n.toFixed(n < 10 ? 1 : 0)} ms`;
}
function compact(n: number): string {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  return `${n}`;
}

export function WorkloadPanel({
  conn,
  onAnalyzeSql,
}: {
  conn: SqlConnectionConfig;
  onAnalyzeSql?: (sql: string) => void;
}) {
  const [rows, setRows] = useState<SlowQuery[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [limit, setLimit] = useState<number>(() => P.load<number>("workload_limit", 25));
  const [copied, setCopied] = useState<number | null>(null);

  const connected = !!conn.server && !!conn.database;

  const load = useCallback(async () => {
    if (!conn.server) { setErr("Connect to a server first."); return; }
    if (!conn.database) { setErr("Pick a database — workload history is per-database."); return; }
    setLoading(true);
    setErr(null);
    try {
      const info = {
        server: conn.server,
        database: conn.database,
        user: conn.auth_mode === "sql" ? conn.user : undefined,
        password: conn.auth_mode === "sql" ? conn.password : undefined,
        trust_cert: conn.trust_cert,
        auth_mode: conn.auth_mode,
      };
      const r = await backend.qstoreTop(info as any, limit);
      setRows(r);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
      setRows(null);
    } finally {
      setLoading(false);
    }
  }, [conn.server, conn.database, conn.user, conn.password, conn.auth_mode, conn.trust_cert, limit]);

  // Auto-load once when this workspace opens on a live connection.
  useEffect(() => {
    if (connected && rows === null && !loading && !err) void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connected]);

  const worst = rows && rows.length ? rows[0].avg_duration_ms : 0;

  return (
    <div className="pane-body" style={{ display: "flex", flexDirection: "column", gap: 12, minHeight: 0 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" }}>
        <button className="btn" onClick={() => void load()} disabled={loading || !connected}>
          {loading ? "READING…" : "↻ REFRESH"}
        </button>
        <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, opacity: 0.8 }}>
          show
          <select
            value={limit}
            onChange={(e) => { const v = Number(e.target.value); setLimit(v); P.save("workload_limit", v); }}
          >
            {[10, 25, 50, 100].map((n) => <option key={n} value={n}>{n}</option>)}
          </select>
          by avg duration
        </label>
        {rows && <span style={{ fontSize: 12, opacity: 0.6 }}>{rows.length} queries · captured history</span>}
      </div>

      {err && (
        <div className="empty-state" style={{ borderLeft: "3px solid #e0533d", padding: "12px 14px" }}>
          <strong style={{ color: "#e0533d" }}>Couldn't read the workload.</strong>
          <div style={{ marginTop: 6, fontSize: 13, opacity: 0.85 }}>{err}</div>
          <div style={{ marginTop: 8, fontSize: 12, opacity: 0.7 }}>
            This needs the database's query-history capture to be on. Turn it on from the connection's capture
            control, let a workload run, then refresh.
          </div>
        </div>
      )}

      {!connected && !err && (
        <div className="empty-state">Connect to a server and pick a database to see its slowest queries.</div>
      )}

      {connected && rows && rows.length === 0 && !err && (
        <div className="empty-state">
          No queries captured yet. Once the database's query-history capture has recorded some workload, the
          slowest statements show up here — ranked, with a one-click path into ANALYZE.
        </div>
      )}

      {rows && rows.length > 0 && (
        <div style={{ overflow: "auto", minHeight: 0 }}>
          <table className="data-table" style={{ width: "100%", borderCollapse: "collapse", fontSize: 13 }}>
            <thead>
              <tr style={{ textAlign: "left", opacity: 0.65, fontSize: 11, letterSpacing: "0.05em" }}>
                <th style={{ padding: "6px 8px" }}>#</th>
                <th style={{ padding: "6px 8px" }}>AVG&nbsp;DURATION</th>
                <th style={{ padding: "6px 8px" }}>CPU</th>
                <th style={{ padding: "6px 8px" }}>READS</th>
                <th style={{ padding: "6px 8px" }}>RUNS</th>
                <th style={{ padding: "6px 8px", width: "50%" }}>QUERY</th>
                <th style={{ padding: "6px 8px" }}></th>
              </tr>
            </thead>
            <tbody>
              {rows.map((q, i) => {
                const frac = worst > 0 ? q.avg_duration_ms / worst : 0;
                return (
                  <tr key={q.query_id} style={{ borderTop: "1px solid var(--hairline, #1c2230)" }}>
                    <td style={{ padding: "8px", opacity: 0.5, fontVariantNumeric: "tabular-nums" }}>{i + 1}</td>
                    <td style={{ padding: "8px", fontVariantNumeric: "tabular-nums", whiteSpace: "nowrap" }}>
                      <span style={{ color: frac > 0.66 ? "#e0533d" : frac > 0.33 ? "#e0a13d" : "var(--signal)" }}>
                        {ms(q.avg_duration_ms)}
                      </span>
                    </td>
                    <td style={{ padding: "8px", fontVariantNumeric: "tabular-nums", opacity: 0.85, whiteSpace: "nowrap" }}>{ms(q.avg_cpu_ms)}</td>
                    <td style={{ padding: "8px", fontVariantNumeric: "tabular-nums", opacity: 0.85, whiteSpace: "nowrap" }}>{compact(q.avg_logical_reads)}</td>
                    <td style={{ padding: "8px", fontVariantNumeric: "tabular-nums", opacity: 0.7 }}>{compact(q.executions)}</td>
                    <td style={{ padding: "8px" }}>
                      <code style={{ fontSize: 12, opacity: 0.9, display: "block", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", maxWidth: "46ch" }}>
                        {q.sql_text.replace(/\s+/g, " ").trim()}
                      </code>
                    </td>
                    <td style={{ padding: "8px", whiteSpace: "nowrap" }}>
                      <button
                        className="btn-ghost"
                        title="Copy the query text"
                        onClick={() => { void navigator.clipboard.writeText(q.sql_text); setCopied(q.query_id); setTimeout(() => setCopied((c) => (c === q.query_id ? null : c)), 1400); }}
                      >
                        {copied === q.query_id ? "✓" : "COPY"}
                      </button>
                      {onAnalyzeSql && (
                        <button
                          className="btn-ghost"
                          title="Send this query to the ANALYZE editor"
                          style={{ marginLeft: 6, color: "var(--signal)" }}
                          onClick={() => onAnalyzeSql(q.sql_text)}
                        >
                          ANALYZE →
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <p style={{ fontSize: 11, opacity: 0.5, marginTop: 10 }}>
            Read-only: these numbers come from the engine's own captured query stats. dbopt never re-runs the
            queries or reads table rows to build this list. A long duration with low CPU usually means the query
            was waiting (blocking / IO), not burning CPU.
          </p>
        </div>
      )}
    </div>
  );
}
