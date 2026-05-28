import { useState } from "react";
import type { Finding } from "../types";

const SEV_LABEL: Record<Finding["severity"], string> = {
  critical: "CRIT",
  error: "ERR",
  warning: "WARN",
  info: "INFO",
};

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
                <span className="rule">{f.rule}</span>
              </div>
              <div className="msg">{f.message}</div>
              {isOpen && f.recommendation && <div className="rec">{f.recommendation}</div>}
            </div>
            <button className="toggle" onClick={() => toggle(i)} aria-label={isOpen ? "collapse" : "expand"}>
              {f.recommendation ? (isOpen ? "−" : "+") : ""}
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
