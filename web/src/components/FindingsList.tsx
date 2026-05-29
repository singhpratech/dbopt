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

// How many jump chips to render per expanded group before collapsing the tail
// into a "+N more" note — keeps a 750-occurrence pattern from spilling the DOM.
const LOC_CAP = 60;

interface RuleGroup {
  rule: string;
  severity: Finding["severity"];
  count: number;
  message: string;
  recommendation: string | null;
  locations: { line: number; col: number }[];
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

function askAiPrompt(g: RuleGroup): string {
  const sample = g.locations.slice(0, 15).map((l) => `L${l.line}`).join(", ");
  return [
    `Fix this recurring anti-pattern across my T-SQL script.`,
    ``,
    `Rule: ${g.rule} (${g.severity})`,
    `Occurrences: ${g.count}${sample ? ` — e.g. ${sample}${g.locations.length > 15 ? ", …" : ""}` : ""}`,
    `What it is: ${g.message}`,
    g.recommendation ? `Analyzer recommendation: ${g.recommendation}` : ``,
    ``,
    `Write ONE canonical rewrite for this pattern, then a short checklist for applying it to all ${g.count} occurrences safely.`,
  ].filter(Boolean).join("\n");
}

export function FindingsList({
  findings,
  onJump,
  onAskAi,
}: {
  findings: Finding[];
  onJump?: (line: number, col: number) => void;
  onAskAi?: (prompt: string) => void;
}) {
  const groups = useMemo(() => groupByRule(findings), [findings]);
  // Default: expand the single highest-priority group so there's something to read.
  const [open, setOpen] = useState<Set<string>>(() => new Set(groups.length ? [groups[0].rule] : []));
  const [copied, setCopied] = useState<string | null>(null);

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

  const counts = { critical: 0, error: 0, warning: 0, info: 0 } as Record<Finding["severity"], number>;
  for (const g of groups) counts[g.severity] += g.count;

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
        <span><strong>{findings.length.toLocaleString()}</strong> findings</span>
        <span className="dot">·</span>
        <span><strong>{groups.length}</strong> {groups.length === 1 ? "pattern" : "patterns"}</span>
        <span className="sev-counts">
          {counts.critical > 0 && <span className="sc crit">{counts.critical} crit</span>}
          {counts.error > 0 && <span className="sc err">{counts.error} err</span>}
          {counts.warning > 0 && <span className="sc warn">{counts.warning} warn</span>}
          {counts.info > 0 && <span className="sc info">{counts.info} info</span>}
        </span>
        <span className="hint">grouped by rule · fix-first order</span>
      </div>

      {groups.map((g, gi) => {
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
              {gi < 3 && <span className="rank" title="One of the top patterns to fix first">#{gi + 1}</span>}
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
                    <button className="fg-btn primary" onClick={() => onAskAi(askAiPrompt(g))}>
                      Ask AI to fix all {g.count.toLocaleString()} →
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
      })}

      <div className="findings-foot">
        {findings.length.toLocaleString()} findings across {groups.length} {groups.length === 1 ? "rule" : "rules"}
      </div>
    </div>
  );
}
