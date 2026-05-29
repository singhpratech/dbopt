import { useId, useLayoutEffect, useRef, useState } from "react";
import type { Confidence, Metric } from "../api/backend";

/**
 * One evidence chip — a grounded label/value pair (e.g. "Writes maintained
 * 412/wk"), pre-formatted server-side and rendered verbatim. Reuses .pill
 * geometry.
 *
 * P1-3 — DRILLABLE: when the chip carries a `source` (the DMV it was measured
 * from) it becomes clickable/hoverable and pops a small provenance card stating
 *   "Measured from sys.dm_db_partition_stats · heuristic estimate"
 * so provenance is per-metric, not only one aggregate confidence badge per
 * issue. Positioned with position:fixed off the trigger box (mirrors <Term>) so
 * it can't be clipped by an overflow:hidden workspace body.
 *
 * Shared by HealthOverview (issue cards) and IssueDetailPane (detail metrics).
 */
export function MetricChip({
  metric,
  confidence,
}: {
  metric: Metric;
  /** The owning issue's confidence band — folded into the popover's tier line. */
  confidence?: Confidence;
}) {
  const id = useId();
  const triggerRef = useRef<HTMLSpanElement>(null);
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ left: number; top: number; below: boolean }>({
    left: 0,
    top: 0,
    below: false,
  });

  // Only the presence of a source (or a confidence band) makes the chip worth a
  // drilldown; without either it's a plain, inert chip (still title-truncated).
  const hasDrill = !!metric.source || !!confidence;

  useLayoutEffect(() => {
    if (!open || !triggerRef.current) return;
    const r = triggerRef.current.getBoundingClientRect();
    const POP_W = 280;
    const ESTIMATED_H = 110;
    const below = r.top < ESTIMATED_H + 16;
    let left = r.left + r.width / 2 - POP_W / 2;
    left = Math.max(10, Math.min(left, window.innerWidth - POP_W - 10));
    const top = below ? r.bottom + 8 : r.top - 8;
    setPos({ left, top, below });
  }, [open]);

  // P2: overflow → title attr (the full label:value) + CSS truncation.
  const titleText = metric.source
    ? `${metric.label}: ${metric.value} — measured from ${metric.source}`
    : `${metric.label}: ${metric.value}`;

  if (!hasDrill) {
    return (
      <span className="metric-chip" title={titleText}>
        <span className="metric-chip-k">{metric.label}</span>
        <span className="metric-chip-v">{metric.value}</span>
      </span>
    );
  }

  return (
    <span
      ref={triggerRef}
      className="metric-chip metric-chip-drill"
      role="button"
      tabIndex={0}
      title={titleText}
      aria-describedby={open ? id : undefined}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      onFocus={() => setOpen(true)}
      onBlur={() => setOpen(false)}
      onKeyDown={(e) => {
        if (e.key === "Escape") setOpen(false);
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          e.stopPropagation();
          setOpen((o) => !o);
        }
      }}
      // Don't also trigger the card's "open detail pane" click.
      onClick={(e) => e.stopPropagation()}
    >
      <span className="metric-chip-k">{metric.label}</span>
      <span className="metric-chip-v">{metric.value}</span>
      {open && (
        <span
          id={id}
          role="tooltip"
          className={`metric-pop${pos.below ? " below" : ""}`}
          style={{
            left: pos.left,
            top: pos.top,
            transform: pos.below ? "none" : "translateY(-100%)",
          }}
          onMouseEnter={() => setOpen(true)}
        >
          <span className="metric-pop-name">{metric.label}</span>
          <span className="metric-pop-val">{metric.value}</span>
          <span className="metric-pop-src">
            {metric.source ? (
              <>
                Measured from <code>{metric.source}</code>
              </>
            ) : (
              "Source not recorded for this metric"
            )}
            {confidence && (
              <>
                {" · "}
                <span className={`metric-pop-tier conf-${confidence}`}>
                  {confidenceTier(confidence)}
                </span>
              </>
            )}
          </span>
        </span>
      )}
    </span>
  );
}

/** Plain-English confidence tier shown after the source in the popover. */
function confidenceTier(c: Confidence): string {
  switch (c) {
    case "observed":
      return "measured value";
    case "estimated":
      return "engine estimate";
    case "heuristic":
      return "heuristic estimate";
  }
}
