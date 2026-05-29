import { useMemo, useState } from "react";
import type { Finding } from "../types";
import { Term, TermText } from "./Term";

const SEV_LABEL: Record<Finding["severity"], string> = {
  critical: "CRIT",
  error: "ERR",
  warning: "WARN",
  info: "INFO",
};

const SEV_WEIGHT: Record<Finding["severity"], number> = { critical: 4, error: 3, warning: 2, info: 1 };
const SEV_ORDER: Finding["severity"][] = ["critical", "error", "warning", "info"];
// Short class suffix so the chips match the .sc.crit/.err/.warn/.info color rules.
const SEV_CLASS: Record<Finding["severity"], string> = { critical: "crit", error: "err", warning: "warn", info: "info" };

// How many jump chips to render per expanded group before collapsing the tail
// into a "+N more" note — keeps a 750-occurrence pattern from spilling the DOM.
const LOC_CAP = 60;

type SortMode = "priority" | "count" | "severity" | "rule";

interface RuleGroup {
  rule: string;
  severity: Finding["severity"];
  count: number;
  message: string;
  recommendation: string | null;
  locations: { line: number; col: number }[];
}

interface Section {
  name: string;
  startLine: number;
  endLine: number;
}

/**
 * Detect meaningful sections in a script — named objects (CREATE/ALTER PROC,
 * FUNCTION, TRIGGER, VIEW). Deliberately conservative: a flat script with no
 * such objects yields ZERO sections, so the Sections control simply doesn't
 * appear (no clutter for the common case).
 */
function parseSections(sql: string): Section[] {
  if (!sql) return [];
  const lines = sql.split("\n");
  const objRe = /^\s*(?:CREATE|ALTER)\s+(?:OR\s+ALTER\s+)?(PROC(?:EDURE)?|FUNCTION|TRIGGER|VIEW)\s+(\[?[\w.$#]+\]?(?:\.\[?[\w.$#]+\]?)?)/i;
  const heads: { name: string; line: number }[] = [];
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(objRe);
    if (m) {
      const kind = m[1].toUpperCase().startsWith("PROC") ? "PROC" : m[1].toUpperCase();
      heads.push({ name: `${kind} ${m[2].replace(/[\[\]]/g, "")}`, line: i + 1 });
    }
  }
  if (heads.length < 2) return []; // 0 or 1 object → not worth a sections UI
  return heads.map((h, i) => ({
    name: h.name,
    startLine: h.line,
    endLine: i + 1 < heads.length ? heads[i + 1].line - 1 : lines.length,
  }));
}

/**
 * Collapse a flat finding list into one group per rule, ranked "fix first" by
 * severity then incidence. A 3000-line script that emits 2,249 findings becomes
 * ~9 actionable patterns instead of an unreadable wall.
 */
function groupByRule(findings: Finding[]): RuleGroup[] {
  const map = new Map<string, RuleGroup>();
  for (const f of findings) {
    let g = map.get(f.rule);
    if (!g) {
      g = { rule: f.rule, severity: f.severity, count: 0, message: f.message, recommendation: f.recommendation, locations: [] };
      map.set(f.rule, g);
    }
    g.count++;
    if (SEV_WEIGHT[f.severity] > SEV_WEIGHT[g.severity]) g.severity = f.severity;
    if (!g.recommendation && f.recommendation) g.recommendation = f.recommendation;
    if (f.location) g.locations.push({ line: f.location.line, col: f.location.col });
  }
  const groups = [...map.values()];
  for (const g of groups) g.locations.sort((a, b) => a.line - b.line);
  groups.sort((a, b) => SEV_WEIGHT[b.severity] - SEV_WEIGHT[a.severity] || b.count - a.count || a.rule.localeCompare(b.rule));
  return groups;
}

function sortGroups(groups: RuleGroup[], mode: SortMode): RuleGroup[] {
  const arr = [...groups];
  switch (mode) {
    case "count": arr.sort((a, b) => b.count - a.count || SEV_WEIGHT[b.severity] - SEV_WEIGHT[a.severity]); break;
    case "severity": arr.sort((a, b) => SEV_WEIGHT[b.severity] - SEV_WEIGHT[a.severity] || a.rule.localeCompare(b.rule)); break;
    case "rule": arr.sort((a, b) => a.rule.localeCompare(b.rule)); break;
    default: arr.sort((a, b) => SEV_WEIGHT[b.severity] - SEV_WEIGHT[a.severity] || b.count - a.count); break; // priority
  }
  return arr;
}

function ruleTerm(rule: string): string | undefined {
  const r = rule.toLowerCase();
  if (r.includes("sarg")) return "sargable";
  if (r.includes("nolock") || r.includes("read_uncommitted") || r.includes("read-uncommitted")) return "blocking";
  if (r.includes("columnstore")) return "columnstore";
  if (r.includes("deadlock")) return "deadlock";
  if (r.includes("blocking")) return "blocking";
  if (r.includes("missing") && r.includes("index")) return "missing_index";
  if (r.includes("unused") && r.includes("index")) return "unused_index";
  if (r.includes("duplicate") && r.includes("index")) return "duplicate_index";
  if (r.includes("heap")) return "heap";
  if (r.includes("cardinalit")) return "cardinality";
  if (r.includes("maxdop")) return "maxdop";
  if (r.includes("scan") || r.includes("seek")) return "scan_vs_seek";
  return undefined;
}

function askAiPrompt(g: RuleGroup, section: Section | null): string {
  const sample = g.locations.slice(0, 15).map((l) => `L${l.line}`).join(", ");
  return [
    `Fix this recurring anti-pattern across my T-SQL script.`,
    ``,
    `Rule: ${g.rule} (${g.severity})`,
    section ? `Scope: ${section.name} (lines ${section.startLine}-${section.endLine})` : ``,
    `Occurrences: ${g.count}${sample ? ` — e.g. ${sample}${g.locations.length > 15 ? ", …" : ""}` : ""}`,
    `What it is: ${g.message}`,
    g.recommendation ? `Analyzer recommendation: ${g.recommendation}` : ``,
    ``,
    `Write ONE canonical rewrite for this pattern, then a short checklist for applying it to all ${g.count} occurrences safely.`,
  ].filter(Boolean).join("\n");
}

export function FindingsList({
  findings,
  sql,
  onJump,
  onAskAi,
}: {
  findings: Finding[];
  sql?: string;
  onJump?: (line: number, col: number) => void;
  onAskAi?: (prompt: string) => void;
}) {
  const allGroups = useMemo(() => groupByRule(findings), [findings]);
  const sections = useMemo(() => parseSections(sql ?? ""), [sql]);

  const [open, setOpen] = useState<Set<string>>(() => new Set(allGroups.length ? [allGroups[0].rule] : []));
  const [copied, setCopied] = useState<string | null>(null);
  const [sort, setSort] = useState<SortMode>("priority");
  const [sevFilter, setSevFilter] = useState<Set<Finding["severity"]>>(new Set());
  const [sectionIdx, setSectionIdx] = useState<number>(-1);

  const activeSection = sectionIdx >= 0 && sectionIdx < sections.length ? sections[sectionIdx] : null;

  // Per-severity totals across the whole script (drives the filter chips).
  const totals = useMemo(() => {
    const t: Record<Finding["severity"], number> = { critical: 0, error: 0, warning: 0, info: 0 };
    for (const g of allGroups) t[g.severity] += g.count;
    return t;
  }, [allGroups]);

  // The groups actually shown: section-scoped (counts recomputed), severity-filtered, sorted.
  const view = useMemo(() => {
    let gs = allGroups;
    if (activeSection) {
      gs = gs
        .map((g) => {
          const locs = g.locations.filter((l) => l.line >= activeSection.startLine && l.line <= activeSection.endLine);
          return { ...g, locations: locs, count: locs.length };
        })
        .filter((g) => g.count > 0);
    }
    if (sevFilter.size) gs = gs.filter((g) => sevFilter.has(g.severity));
    return sortGroups(gs, sort);
  }, [allGroups, activeSection, sevFilter, sort]);

  const shownFindings = view.reduce((n, g) => n + g.count, 0);
  const filtering = sevFilter.size > 0 || activeSection != null;

  if (findings.length === 0) {
    return (
      <div className="empty">
        <div className="empty-card">
          <div className="empty-glyph">∅</div>
          <div className="empty-title">Awaiting input</div>
          <div className="empty-hint">
            Paste a T-SQL script in the editor, or drop a <code>.sqlplan</code> file to begin analysis.
          </div>
        </div>
      </div>
    );
  }

  function toggle(rule: string) {
    setOpen((prev) => {
      const next = new Set(prev);
      next.has(rule) ? next.delete(rule) : next.add(rule);
      return next;
    });
  }

  function toggleSev(s: Finding["severity"]) {
    setSevFilter((prev) => {
      const next = new Set(prev);
      next.has(s) ? next.delete(s) : next.add(s);
      return next;
    });
  }

  function pickSection(idx: number) {
    setSectionIdx(idx);
    if (idx >= 0 && sections[idx]) onJump?.(sections[idx].startLine, 1);
  }

  function clearFilters() {
    setSevFilter(new Set());
    setSectionIdx(-1);
  }

  async function copyFix(rule: string, rec: string) {
    try {
      await navigator.clipboard.writeText(rec);
      setCopied(rule);
      setTimeout(() => setCopied((c) => (c === rule ? null : c)), 1500);
    } catch { /* clipboard blocked — no-op */ }
  }

  return (
    <div className="findings findings-grouped">
      <div className="findings-summary">
        <div className="fs-row">
          <span>
            <strong>{filtering ? `${shownFindings.toLocaleString()} / ${findings.length.toLocaleString()}` : findings.length.toLocaleString()}</strong> findings
          </span>
          <span className="dot">·</span>
          <span><strong>{view.length}</strong> {view.length === 1 ? "pattern" : "patterns"}</span>
          <span className="sev-counts">
            {SEV_ORDER.filter((s) => totals[s] > 0).map((s) => (
              <button
                key={s}
                className={`sc ${SEV_CLASS[s]} ${sevFilter.has(s) ? "on" : ""}`}
                onClick={() => toggleSev(s)}
                title={`Filter to ${SEV_LABEL[s]} (toggle)`}
              >
                {totals[s]} {SEV_LABEL[s].toLowerCase()}
              </button>
            ))}
          </span>
        </div>
        <div className="fs-row controls">
          <label className="fs-ctl">
            sort
            <select value={sort} onChange={(e) => setSort(e.target.value as SortMode)}>
              <option value="priority">fix-first</option>
              <option value="count">most occurrences</option>
              <option value="severity">severity</option>
              <option value="rule">rule name</option>
            </select>
          </label>
          {sections.length >= 2 && (
            <label className="fs-ctl">
              section
              <select value={sectionIdx} onChange={(e) => pickSection(Number(e.target.value))}>
                <option value={-1}>whole script</option>
                {sections.map((s, i) => (
                  <option key={i} value={i}>{s.name} (L{s.startLine}–{s.endLine})</option>
                ))}
              </select>
            </label>
          )}
          {filtering && (
            <button className="fs-clear" onClick={clearFilters} title="Clear filters">clear ✕</button>
          )}
        </div>
      </div>

      {view.length === 0 ? (
        <div className="fg-empty">No findings match the current filter.</div>
      ) : (
        view.map((g, gi) => {
          const isOpen = open.has(g.rule);
          return (
            <div key={g.rule} className={`finding-group sev-${g.severity} ${isOpen ? "expanded" : ""}`}>
              <div
                className="fg-head"
                role="button"
                tabIndex={0}
                aria-expanded={isOpen}
                onClick={() => toggle(g.rule)}
                onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(g.rule); } }}
              >
                <span className="gutter" />
                {sort === "priority" && !filtering && gi < 3 && <span className="rank" title="Top pattern to fix first">#{gi + 1}</span>}
                <span className="sev-tag">{SEV_LABEL[g.severity]}</span>
                <span className="rule"><Term k={ruleTerm(g.rule) ?? "__none__"}>{g.rule}</Term></span>
                <span className="count">{g.count.toLocaleString()}×</span>
                <span className="msg"><TermText>{g.message}</TermText></span>
                <span className="caret">{isOpen ? "−" : "＋"}</span>
              </div>

              {isOpen && (
                <div className="fg-body">
                  {g.recommendation && (
                    <div className="rec">
                      <div className="rec-label">Recommended fix</div>
                      <div className="rec-body"><TermText>{g.recommendation}</TermText></div>
                    </div>
                  )}

                  <div className="fg-actions">
                    {onAskAi && (
                      <button className="fg-btn primary" onClick={() => onAskAi(askAiPrompt(g, activeSection))}>
                        Ask AI to fix all {g.count.toLocaleString()}{activeSection ? " here" : ""} →
                      </button>
                    )}
                    {g.recommendation && (
                      <button className="fg-btn" onClick={() => copyFix(g.rule, g.recommendation!)}>
                        {copied === g.rule ? "Copied ✓" : "Copy fix"}
                      </button>
                    )}
                  </div>

                  {g.locations.length > 0 && (
                    <div className="fg-locs">
                      <span className="fg-locs-label">{g.count.toLocaleString()} location{g.count === 1 ? "" : "s"}:</span>
                      {g.locations.slice(0, LOC_CAP).map((loc, i) => (
                        <button
                          key={i}
                          className="loc-chip"
                          onClick={() => onJump?.(loc.line, loc.col)}
                          title="Jump to this line in the editor"
                        >
                          L{loc.line}
                        </button>
                      ))}
                      {g.locations.length > LOC_CAP && (
                        <span className="fg-locs-more">+{(g.locations.length - LOC_CAP).toLocaleString()} more</span>
                      )}
                    </div>
                  )}
                </div>
              )}
            </div>
          );
        })
      )}

      <div className="findings-foot">
        {filtering
          ? `${shownFindings.toLocaleString()} of ${findings.length.toLocaleString()} findings shown`
          : `${findings.length.toLocaleString()} findings across ${allGroups.length} ${allGroups.length === 1 ? "rule" : "rules"}`}
      </div>
    </div>
  );
}
