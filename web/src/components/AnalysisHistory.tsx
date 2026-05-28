import { useEffect, useState } from "react";
import * as runlog from "../store/runlog";
import type { RunEntry } from "../store/runlog";

export function AnalysisHistory(props: { server: string | null; database: string | null }) {
  const [, rev] = useState(0);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [findings, setFindings] = useState<Awaited<ReturnType<typeof runlog.fetchFindings>>>([]);

  useEffect(() => {
    runlog.hydrate({ server: props.server ?? undefined, database: props.database ?? undefined });
    return runlog.subscribe(() => rev((x) => x + 1));
  }, [props.server, props.database]);

  const runs = runlog.getAll();

  async function open(id: string) {
    if (expanded === id) { setExpanded(null); setFindings([]); return; }
    setExpanded(id);
    setFindings(await runlog.fetchFindings(id));
  }

  return (
    <div className="logger">
      <div className="logger-head">
        <span>
          <span className="count">{runs.length}</span>&nbsp; analysis run{runs.length === 1 ? "" : "s"} ·{" "}
          {props.server ? <em>{props.server}</em> : <em>any server</em>}{" "}
          {props.database ? <>· <em>{props.database}</em></> : null}
        </span>
        <div className="actions">
          <button onClick={() => runlog.hydrate({ server: props.server ?? undefined, database: props.database ?? undefined })}>Refresh</button>
        </div>
      </div>

      {runs.length === 0 ? (
        <div className="empty">
          <div className="empty-card">
            <div className="empty-glyph">⎯</div>
            <div className="empty-title">No analysis runs recorded yet</div>
            <div className="empty-hint">Every analyzer invocation — ad-hoc script or full database scan — is logged here durably (SQLite). Survives backend restart + browser cache clear.</div>
          </div>
        </div>
      ) : (
        <div className="logger-table">
          <table>
            <colgroup>
              <col style={{ width: "13%" }} />
              <col style={{ width: "9%" }} />
              <col style={{ width: "14%" }} />
              <col style={{ width: "14%" }} />
              <col style={{ width: "10%" }} />
              <col style={{ width: "10%" }} />
              <col style={{ width: "10%" }} />
              <col style={{ width: "20%" }} />
            </colgroup>
            <thead>
              <tr>
                <th>Time</th>
                <th>Mode</th>
                <th>Server</th>
                <th>Database</th>
                <th>Findings</th>
                <th>Plan cost</th>
                <th>Duration</th>
                <th>SQL preview</th>
              </tr>
            </thead>
            <tbody>
              {runs.map((r) => (
                <RunRow
                  key={r.id}
                  run={r}
                  expanded={expanded === r.id}
                  onToggle={() => open(r.id)}
                  findings={expanded === r.id ? findings : null}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function RunRow({ run, expanded, onToggle, findings }: {
  run: RunEntry;
  expanded: boolean;
  onToggle: () => void;
  findings: Awaited<ReturnType<typeof runlog.fetchFindings>> | null;
}) {
  const time = new Date(run.occurred_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  const date = new Date(run.occurred_at).toLocaleDateString();
  const totals = (
    <span className="sev-summary" title={`${run.findings_total} findings total`}>
      {run.findings_critical > 0 && <span className="pill crit">{run.findings_critical}C</span>}
      {run.findings_error > 0 && <span className="pill err">{run.findings_error}E</span>}
      {run.findings_warning > 0 && <span className="pill warn">{run.findings_warning}W</span>}
      {run.findings_info > 0 && <span className="pill info">{run.findings_info}I</span>}
      {run.findings_total === 0 && <span className="muted">—</span>}
    </span>
  );

  return (
    <>
      <tr onClick={onToggle} className={expanded ? "row-open" : ""}>
        <td><span className="muted">{date}</span> {time}</td>
        <td><span className={"pill " + (run.mode === "adhoc" ? "info" : "warn")}>{run.mode}</span></td>
        <td>{run.server_name ?? <span className="muted">—</span>}</td>
        <td>{run.database_name ?? <span className="muted">—</span>}</td>
        <td>{totals}</td>
        <td>{run.plan_subtree_cost != null ? run.plan_subtree_cost.toLocaleString(undefined, { maximumFractionDigits: 1 }) : <span className="muted">—</span>}</td>
        <td>{run.duration_ms != null ? `${run.duration_ms} ms` : <span className="muted">—</span>}</td>
        <td className="sql-prev">{run.sql_preview ?? <span className="muted">—</span>}</td>
      </tr>
      {expanded && findings && (
        <tr className="row-detail">
          <td colSpan={8}>
            <div className="findings-inline">
              {findings.length === 0 ? <em className="muted">no findings recorded</em> : findings.map((f, i) => (
                <div key={i} className={`finding-row sev-${f.severity}`}>
                  <span className="pill-mini">{f.severity}</span>
                  <span className="rule-id">{f.rule}</span>
                  {f.line != null && <span className="muted">L{f.line}:{f.col ?? 0}</span>}
                  <span>{f.message}</span>
                </div>
              ))}
            </div>
          </td>
        </tr>
      )}
    </>
  );
}
