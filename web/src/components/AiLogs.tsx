import { useEffect, useState } from "react";
import * as ailog from "../store/ailog";

export function AiLogs() {
  const [, rev] = useState(0);
  const [open, setOpen] = useState<string | null>(null);
  const [redact, setRedact] = useState(true);

  useEffect(() => ailog.subscribe(() => rev((x) => x + 1)), []);

  const entries = ailog.getAll();

  function dl(kind: "json" | "csv") {
    const stamp = new Date().toISOString().replace(/[:.]/g, "-");
    if (kind === "json") {
      ailog.download(`sqlopt-ailog-${stamp}.json`, "application/json", ailog.exportJson(redact));
    } else {
      ailog.download(`sqlopt-ailog-${stamp}.csv`, "text/csv", ailog.exportCsv(redact));
    }
  }

  return (
    <div className="logger">
      <div className="logger-head">
        <span><span className="count">{entries.length}</span> &nbsp;interaction{entries.length === 1 ? "" : "s"} · capped at 500</span>
        <label className="form-row cb" style={{ flexDirection: "row" }} title="Redact API keys + bearer tokens in exports">
          <input type="checkbox" checked={redact} onChange={(e) => setRedact(e.target.checked)} /> Redact keys on export
        </label>
        <div className="actions">
          <button onClick={() => dl("json")}>Download JSON</button>
          <button onClick={() => dl("csv")}>Download CSV</button>
          <button onClick={() => { if (confirm("Clear all logged interactions?")) ailog.clear(); }}>Clear</button>
        </div>
      </div>

      {entries.length === 0 ? (
        <div className="empty">
          <div className="empty-card">
            <div className="empty-glyph">⎯</div>
            <div className="empty-title">No interactions yet</div>
            <div className="empty-hint">Every prompt and response from any LLM provider will be recorded here for audit & export.</div>
          </div>
        </div>
      ) : (
        <div className="logger-table">
          <table>
            <colgroup>
              <col style={{ width: "10%" }} />
              <col style={{ width: "8%" }} />
              <col style={{ width: "12%" }} />
              <col style={{ width: "18%" }} />
              <col style={{ width: "8%" }} />
              <col style={{ width: "8%" }} />
              <col style={{ width: "8%" }} />
              <col style={{ width: "28%" }} />
            </colgroup>
            <thead>
              <tr>
                <th>Time</th>
                <th>Status</th>
                <th>Provider</th>
                <th>Model</th>
                <th>Latency</th>
                <th>In</th>
                <th>Out</th>
                <th>Preview</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((e) => {
                const isOpen = open === e.id;
                return (
                  <FragmentRow key={e.id}>
                    <tr
                      className={isOpen ? "expanded" : ""}
                      onClick={() => setOpen(isOpen ? null : e.id)}
                      style={{ cursor: "pointer" }}
                    >
                      <td>{new Date(e.timestamp).toLocaleTimeString()}</td>
                      <td className={`dir ${e.status === "ok" ? "out" : e.status === "error" ? "err" : "in"}`}>
                        {e.status.toUpperCase()}
                      </td>
                      <td>{e.provider}</td>
                      <td>{e.model}</td>
                      <td>{e.latency_ms != null ? `${e.latency_ms} ms` : "—"}</td>
                      <td>{e.tokens_in ?? "—"}</td>
                      <td>{e.tokens_out ?? "—"}</td>
                      <td className="preview">{e.response ? e.response.slice(0, 120).replace(/\n/g, " ") : (e.error ?? "")}</td>
                    </tr>
                    {isOpen && (
                      <tr className="expand-row">
                        <td colSpan={8}>
                          <div className="expand-panel">
                            {e.system && (<><div className="lk">System prompt</div><div className="lv">{e.system}</div></>)}
                            <div className="lk">User</div>
                            <div className="lv">{e.user}</div>
                            <div className="lk">Response {e.status === "error" ? "(error)" : ""}</div>
                            <div className="lv">{e.error ?? e.response}</div>
                          </div>
                        </td>
                      </tr>
                    )}
                  </FragmentRow>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function FragmentRow({ children }: { children: React.ReactNode }) {
  return <>{children}</>;
}
