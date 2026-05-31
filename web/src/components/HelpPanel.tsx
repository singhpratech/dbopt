import { useEffect, useMemo, useRef, useState } from "react";
import { GLOSSARY } from "../glossary";

/**
 * The "?" help slide-over. Two sections:
 *   1. "How dbopt works" — the 4-step mental model.
 *   2. "Glossary" — every GLOSSARY entry as a searchable term + definition row.
 *
 * Chrome reuses the issue-detail slide-over pattern (right-side pane, no
 * backdrop dimming required) styled via .help-panel in index.css. ESC or the ×
 * button closes. `focusTerm` (optional) opens scrolled to a specific glossary
 * row and highlights it — used when a "?" next to a grade/label is clicked.
 */

export const HELP_STEPS: { n: string; title: string; body: string }[] = [
  {
    n: "①",
    title: "Connect your SQL Server",
    body: "Point dbopt at an instance (host, login, database). Nothing leaves your machine — it talks to your server directly.",
  },
  {
    n: "②",
    title: "We scan DMVs + your workload",
    body: "dbopt reads SQL Server's built-in Dynamic Management Views to see what's running, what's waiting, and how indexes are used.",
  },
  {
    n: "③",
    title: "See a ranked health report",
    body: "You get plain-English grades (reliability + efficiency) and a list of issues ordered by how much they actually matter.",
  },
  {
    n: "④",
    title: "Copy the fix / apply safely",
    body: "Each issue comes with copy-paste T-SQL and a safe apply path — so you can fix it yourself or hand it to whoever owns the database.",
  },
];

/**
 * The unified A–F grade rubric — defined in ONE place (here) so the grade
 * letters mean the same thing everywhere. Linked from HealthOverview's grade
 * explainer via onOpenHelp("grade_rubric").
 */
export const GRADE_RUBRIC: { grade: string; band: string; cls: string }[] = [
  { grade: "A", band: "90–100", cls: "grade-a" },
  { grade: "B", band: "80–89", cls: "grade-b" },
  { grade: "C", band: "70–79", cls: "grade-c" },
  { grade: "D", band: "60–69", cls: "grade-d" },
  { grade: "F", band: "<60", cls: "grade-f" },
];

/** Sentinel focusTerm value that scrolls the Help panel to the grade rubric. */
export const GRADE_RUBRIC_FOCUS = "grade_rubric";

export function HelpPanel({
  open,
  onClose,
  focusTerm,
}: {
  open: boolean;
  onClose: () => void;
  focusTerm?: string;
}) {
  const [q, setQ] = useState("");
  const bodyRef = useRef<HTMLDivElement>(null);
  const focusRowRef = useRef<HTMLDivElement>(null);
  const rubricRef = useRef<HTMLElement>(null);

  // ESC closes from anywhere while open.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  // When opened with a focusTerm, scroll it into view. The rubric anchor and
  // the two grade slugs both resolve to the rubric section; any other slug
  // scrolls to its glossary row.
  const focusRubric =
    focusTerm === GRADE_RUBRIC_FOCUS ||
    focusTerm === "reliability_grade" ||
    focusTerm === "efficiency_grade";
  useEffect(() => {
    if (!open || !focusTerm) return;
    const t = setTimeout(() => {
      const el = focusRubric ? rubricRef.current : focusRowRef.current;
      el?.scrollIntoView({ block: "center", behavior: "smooth" });
    }, 120);
    return () => clearTimeout(t);
  }, [open, focusTerm, focusRubric]);

  const entries = useMemo(() => {
    const all = Object.entries(GLOSSARY);
    const needle = q.trim().toLowerCase();
    if (!needle) return all;
    return all.filter(
      ([slug, e]) =>
        slug.includes(needle) ||
        e.term.toLowerCase().includes(needle) ||
        e.short.toLowerCase().includes(needle)
    );
  }, [q]);

  return (
    <>
      <div
        className={`help-scrim${open ? " open" : ""}`}
        onClick={onClose}
        aria-hidden
      />
      <aside
        className={`help-panel${open ? " open" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label="Help and glossary"
      >
        <header className="help-panel-header">
          <span className="help-panel-eyebrow">Help &amp; glossary</span>
          <h2 className="help-panel-title">How dbopt works</h2>
          <button
            className="ddl-copy help-panel-close"
            onClick={onClose}
            title="Close (Esc)"
            aria-label="Close help"
          >
            ✕
          </button>
        </header>

        <div className="help-panel-body" ref={bodyRef}>
          <section className="help-steps">
            {HELP_STEPS.map((s) => (
              <div className="help-step" key={s.n}>
                <span className="help-step-n">{s.n}</span>
                <div className="help-step-text">
                  <div className="help-step-title">{s.title}</div>
                  <div className="help-step-body">{s.body}</div>
                </div>
              </div>
            ))}
          </section>

          {/* A5: the ONE place the A–F rubric + grade dimensions + provenance
              tiers are defined, so the letters mean the same thing everywhere.
              Linked from the HEALTH grade explainer. */}
          <section
            className={`help-rubric${focusRubric ? " focus" : ""}`}
            ref={rubricRef}
            aria-label="How grades work"
          >
            <h3 className="help-section-title">How grades work</h3>
            <p className="help-rubric-lead">
              Two grades, two questions. <strong>Reliability</strong> asks{" "}
              <em>“are users hitting errors right now?”</em> (deadlocks, blocking, harmful
              waits, regressions). <strong>Efficiency</strong> asks{" "}
              <em>“how much speed and cost could you reclaim?”</em> — a lower efficiency grade
              means more easy wins are available, not that anything is broken.
            </p>
            <div className="help-rubric-scale">
              {GRADE_RUBRIC.map((r) => (
                <div className="help-rubric-row" key={r.grade}>
                  <span className={`pill ${r.cls} help-rubric-grade`}>{r.grade}</span>
                  <span className="help-rubric-band">{r.band}</span>
                </div>
              ))}
            </div>
            <p className="help-rubric-prov">
              Every number carries a provenance tier so we never imply fake precision:{" "}
              <span className="conf-observed help-rubric-tier">✓ observed</span> — measured
              directly from DMV counters;{" "}
              <span className="conf-estimated help-rubric-tier">○ estimated</span> — SQL
              Server's own projection;{" "}
              <span className="conf-heuristic help-rubric-tier">⚡ heuristic</span> — a
              rule-of-thumb, verify before acting.
            </p>
          </section>

          {/* B3: Continuous monitoring (Sentinel) — explains WATCH/Sentinel and
              why the runtime signals read "not monitored yet" until it's run. */}
          <section className="help-sentinel" aria-label="Continuous monitoring (Sentinel)">
            <h3 className="help-section-title">Continuous monitoring (Sentinel)</h3>
            <p className="help-sentinel-lead">
              A one-off scan reads SQL Server's <strong>current</strong> DMV counters — great for
              structural issues (missing / unused / duplicate indexes, columnstore candidates),
              which are always measurable on the spot. But <strong>runtime</strong> problems —
              deadlocks, blocking, harmful waits, query regressions — only show up over TIME. A
              single snapshot can't see a deadlock that happened last Tuesday at 2am.
            </p>
            <p className="help-sentinel-lead">
              <strong>Sentinel</strong> (started with <code>WATCH</code>) is dbopt's continuous
              poller. It samples those views on an interval and keeps a local time-series in{" "}
              <code>~/.dbopt/sentinel.db</code>, so runtime pain accumulates into real history you
              can grade against — and into the weekly pain report.
            </p>
            <p className="help-sentinel-why">
              <span className="health-signal-eye" aria-hidden>
                ◎
              </span>
              That's why runtime signals say <em>“not monitored yet”</em> instead of a reassuring{" "}
              <strong>0</strong>: with no Sentinel history, the absence of a deadlock isn't proof
              there were none — we just haven't been watching. Start Sentinel and let it run; the
              learning-mode banner shows how long until grades firm up (~7 days of baseline).
            </p>
          </section>

          <section className="help-glossary">
            <div className="help-glossary-head">
              <h3 className="help-section-title">Glossary</h3>
              <input
                className="help-search"
                type="search"
                placeholder="Filter terms…"
                value={q}
                onChange={(e) => setQ(e.target.value)}
                spellCheck={false}
                autoCorrect="off"
                autoCapitalize="off"
              />
            </div>

            {entries.length === 0 ? (
              <div className="help-glossary-empty">
                No terms match “{q}”.
              </div>
            ) : (
              <div className="help-terms">
                {entries.map(([slug, e]) => {
                  const isFocus = focusTerm === slug;
                  return (
                    <div
                      key={slug}
                      ref={isFocus ? focusRowRef : undefined}
                      className={`help-term${isFocus ? " focus" : ""}`}
                    >
                      <div className="help-term-name">{e.term}</div>
                      <div className="help-term-def">{e.long ?? e.short}</div>
                      {e.docUrl && (
                        <a
                          className="help-term-link"
                          href={e.docUrl}
                          target="_blank"
                          rel="noopener noreferrer"
                        >
                          Learn more ↗
                        </a>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </section>
        </div>
      </aside>
    </>
  );
}
