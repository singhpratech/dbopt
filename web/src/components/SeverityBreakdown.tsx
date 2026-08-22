import { useMemo } from "react";
import type { Finding, Severity } from "../types";
import type { UiPrefs } from "../store/persist";

/**
 * SEVERITY workspace — the triage table behind the topbar counter.
 *
 * Replaces the old per-line stacked bar (a one-line query became two colour
 * blocks, and plan findings — which have no source line — were silently
 * dropped). This shows EVERY finding, counted two ways and clickable:
 *
 *   • by severity  — four tiles (critical / error / warning / info);
 *   • by source    — where the finding came from: the T-SQL text (rule fired on
 *                    a token at a line), the plan XML (`plan.*`, no line), or
 *                    the DMV / schema bundle (structure + usage rules, no line).
 *
 * Each row jumps: text findings go to the editor line in ANALYZE; plan findings
 * open PLAN; DMV findings open INDEX. Nothing here is estimated — counts only.
 */
type Source = "sql" | "plan" | "dmv";

const SEVERITIES: Severity[] = ["critical", "error", "warning", "info"];
const SOURCES: { key: Source; label: string; detail: string; workspace: UiPrefs["workspace"] }[] = [
  { key: "sql", label: "T-SQL text", detail: "rule matched the query text — click to jump to the line", workspace: "analyze" },
  { key: "plan", label: "Execution plan", detail: "rule read the plan XML — operators, not lines", workspace: "plan" },
  { key: "dmv", label: "DMV / schema", detail: "rule read the pulled catalog + usage counters", workspace: "indexes" },
];

function sourceOf(f: Finding): Source {
  if (f.location) return "sql";
  if (f.rule.startsWith("plan.")) return "plan";
  return "dmv";
}

export function SeverityBreakdown({
  findings,
  sql,
  hasPlan,
  hasDmv,
  onJumpToSql,
  onOpen,
}: {
  findings: Finding[];
  sql: string;
  hasPlan: boolean;
  hasDmv: boolean;
  onJumpToSql: (line: number, col: number) => void;
  onOpen: (ws: UiPrefs["workspace"]) => void;
}) {
  const model = useMemo(() => {
    const bySev: Record<Severity, number> = { critical: 0, error: 0, warning: 0, info: 0 };
    const bySrc: Record<Source, Record<Severity, number>> = {
      sql: { critical: 0, error: 0, warning: 0, info: 0 },
      plan: { critical: 0, error: 0, warning: 0, info: 0 },
      dmv: { critical: 0, error: 0, warning: 0, info: 0 },
    };
    // rule → one row per (source, rule) with its hits, severity-sorted.
    const rows = new Map<string, { rule: string; source: Source; severity: Severity; hits: Finding[] }>();
    for (const f of findings) {
      const src = sourceOf(f);
      bySev[f.severity]++;
      bySrc[src][f.severity]++;
      const k = `${src}:${f.rule}`;
      const r = rows.get(k) ?? { rule: f.rule, source: src, severity: f.severity, hits: [] };
      r.hits.push(f);
      if (SEVERITIES.indexOf(f.severity) < SEVERITIES.indexOf(r.severity)) r.severity = f.severity;
      rows.set(k, r);
    }
    const sorted = [...rows.values()].sort(
      (a, b) => SEVERITIES.indexOf(a.severity) - SEVERITIES.indexOf(b.severity) || b.hits.length - a.hits.length,
    );
    return { bySev, bySrc, rows: sorted };
  }, [findings]);

  const total = findings.length;
  const inputs = [
    sql.trim() ? "T-SQL" : null,
    hasPlan ? "plan" : null,
    hasDmv ? "DMVs" : null,
  ].filter(Boolean);

  if (total === 0) {
    return (
      <div className="empty">
        <div className="empty-card">
          <div className="empty-glyph">≡</div>
          <div className="empty-title">{inputs.length ? "No findings to triage" : "Nothing analyzed yet"}</div>
          <div className="empty-hint">
            {inputs.length
              ? `The current input (${inputs.join(" + ")}) analyzed clean — there is nothing to rank.`
              : "Paste T-SQL, load a plan, or pull DMVs in ANALYZE; every finding lands here, counted by severity and by source."}
          </div>
          <div className="empty-action">
            <button className="btn primary" onClick={() => onOpen("analyze")}>Open ANALYZE</button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="sev-wrap">
      {/* ── By severity ───────────────────────────── */}
      <div className="sev-tiles" role="list" aria-label="Findings by severity">
        {SEVERITIES.map((s) => (
          <div key={s} role="listitem" className={`sev-tile sev-${s}${model.bySev[s] === 0 ? " zero" : ""}`}>
            <span className="sev-tile-n">{model.bySev[s]}</span>
            <span className="sev-tile-k">{s}</span>
          </div>
        ))}
        <div className="sev-tile total">
          <span className="sev-tile-n">{total}</span>
          <span className="sev-tile-k">findings · {model.rows.length} rule{model.rows.length === 1 ? "" : "s"}</span>
        </div>
      </div>

      {/* ── By source × severity ───────────────────── */}
      <div className="sev-matrix" role="table" aria-label="Findings by source and severity">
        <div className="sev-matrix-head" role="row">
          <span role="columnheader">Source</span>
          {SEVERITIES.map((s) => <span key={s} role="columnheader" className={`sev-col sev-${s}`}>{s.slice(0, 4)}</span>)}
          <span role="columnheader" className="sev-col">all</span>
        </div>
        {SOURCES.map((src) => {
          const n = SEVERITIES.reduce((a, s) => a + model.bySrc[src.key][s], 0);
          const present = src.key === "sql" ? !!sql.trim() : src.key === "plan" ? hasPlan : hasDmv;
          return (
            <div key={src.key} role="row" className={`sev-matrix-row${n === 0 ? " zero" : ""}`}>
              <span role="cell" className="sev-src">
                <button className="sev-src-btn" onClick={() => onOpen(src.workspace)} title={`Open ${src.workspace.toUpperCase()}`}>
                  {src.label}
                </button>
                <span className="sev-src-detail">{present ? src.detail : "no input loaded"}</span>
              </span>
              {SEVERITIES.map((s) => (
                <span key={s} role="cell" className={`sev-col sev-${s}${model.bySrc[src.key][s] ? "" : " zero"}`}>
                  {model.bySrc[src.key][s]}
                </span>
              ))}
              <span role="cell" className="sev-col">{n}</span>
            </div>
          );
        })}
      </div>

      {/* ── Every rule, click to jump ──────────────── */}
      <div className="sev-rows">
        {model.rows.map((r) => (
          <div key={`${r.source}:${r.rule}`} className={`sev-row sev-${r.severity}`}>
            <span className="sev-row-sev" aria-label={r.severity}>{r.severity}</span>
            <span className="sev-row-rule">{r.rule}</span>
            <span className="sev-row-src">{SOURCES.find((x) => x.key === r.source)!.label}</span>
            <span className="sev-row-msg" title={r.hits[0].message}>{r.hits[0].message}</span>
            <span className="sev-row-jumps">
              {r.source === "sql"
                ? r.hits.slice(0, 8).map((h, i) => (
                    <button
                      key={i}
                      className="sev-jump"
                      onClick={() => onJumpToSql(h.location!.line, h.location!.col)}
                      title={`Jump to line ${h.location!.line} in the editor`}
                    >
                      L{h.location!.line}
                    </button>
                  ))
                : (
                    <button
                      className="sev-jump"
                      onClick={() => onOpen(SOURCES.find((x) => x.key === r.source)!.workspace)}
                      title={r.source === "plan" ? "Open the plan treemap" : "Open the index usage view"}
                    >
                      {r.hits.length > 1 ? `${r.hits.length}× · ` : ""}{r.source === "plan" ? "PLAN →" : "INDEX →"}
                    </button>
                  )}
              {r.source === "sql" && r.hits.length > 8 && <span className="sev-more">+{r.hits.length - 8}</span>}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
