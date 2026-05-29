import { useState } from "react";
import type { Finding } from "../types";
import { Term, TermText } from "./Term";

const SEV_LABEL: Record<Finding["severity"], string> = {
  critical: "CRIT",
  error: "ERR",
  warning: "WARN",
  info: "INFO",
};

/**
 * Map a rule id (e.g. "non-sargable-predicate", "nolock-hint") onto a glossary
 * slug so the rule chip gets a hover definition. Returns undefined when no term
 * applies — <Term> with an unknown key renders its children plain anyway, so
 * this is purely an optimisation/intent marker.
 */
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

export function FindingsList({
  findings,
  onJump,
}: {
  findings: Finding[];
  onJump?: (line: number, col: number) => void;
}) {
  const [open, setOpen] = useState<Set<number>>(new Set());

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

  function toggle(i: number) {
    setOpen((prev) => {
      const next = new Set(prev);
      next.has(i) ? next.delete(i) : next.add(i);
      return next;
    });
  }

  return (
    <div className="findings">
      {findings.map((f, i) => {
        const isOpen = open.has(i);
        return (
          <div key={i} className={`finding sev-${f.severity} ${isOpen ? "expanded" : ""}`}>
            <div className="gutter" />
            <div
              className="loc"
              onClick={() => f.location && onJump?.(f.location.line, f.location.col)}
              title={f.location ? "Jump to source" : ""}
              style={{ cursor: f.location ? "pointer" : "default" }}
            >
              {f.location ? (
                <>
                  <span className="line">L{f.location.line}</span>:<span>{f.location.col}</span>
                </>
              ) : (
                <span>—</span>
              )}
            </div>
            <div className="body">
              <div className="head">
                <span className="sev-tag">{SEV_LABEL[f.severity]}</span>
                <span className="rule">
                  <Term k={ruleTerm(f.rule) ?? "__none__"}>{f.rule}</Term>
                </span>
              </div>
              <div className="msg"><TermText>{f.message}</TermText></div>
              {isOpen && f.recommendation && (
                <div className="rec">
                  <div className="rec-label">Recommended fix</div>
                  <div className="rec-body"><TermText>{f.recommendation}</TermText></div>
                </div>
              )}
            </div>
            <button
              className={`toggle ${f.recommendation ? "expandable" : "disabled"}`}
              onClick={() => f.recommendation && toggle(i)}
              disabled={!f.recommendation}
              aria-expanded={f.recommendation ? isOpen : undefined}
              aria-label={
                f.recommendation
                  ? isOpen
                    ? "Collapse recommendation"
                    : "Show recommendation"
                  : "No recommendation"
              }
              title={f.recommendation ? (isOpen ? "Hide fix" : "Show fix") : "No recommendation"}
            >
              {f.recommendation ? (isOpen ? "−" : "＋") : "–"}
            </button>
          </div>
        );
      })}
      <div className="findings-foot">
        {findings.length} {findings.length === 1 ? "finding" : "findings"}
      </div>
    </div>
  );
}
