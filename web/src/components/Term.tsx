import { useId, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { GLOSSARY } from "../glossary";

/**
 * Surface-form → glossary-slug map for auto-wrapping jargon found inside
 * free-text strings (finding messages, advisor rationales). The keys are
 * matched case-insensitively as whole words/phrases by <TermText>. Several
 * surface forms can point at the same slug (e.g. "NOLOCK"/"READ UNCOMMITTED").
 */
const JARGON: { pattern: RegExp; slug: string }[] = [
  { pattern: /\bnon-?SARGable\b/gi, slug: "sargable" },
  { pattern: /\bSARGabilit(?:y|ies)\b/gi, slug: "sargable" },
  { pattern: /\bSARGable\b/gi, slug: "sargable" },
  { pattern: /\bNOLOCK\b/gi, slug: "blocking" },
  { pattern: /\bREAD UNCOMMITTED\b/gi, slug: "blocking" },
  { pattern: /\bcolumnstore\b/gi, slug: "columnstore" },
  { pattern: /\bdeadlocks?\b/gi, slug: "deadlock" },
  { pattern: /\bblocking\b/gi, slug: "blocking" },
  { pattern: /\bcardinalit(?:y|ies)\b/gi, slug: "cardinality" },
  { pattern: /\bwait type\b/gi, slug: "wait_type" },
  { pattern: /\bregressions?\b/gi, slug: "regression" },
  { pattern: /\bmissing index(?:es)?\b/gi, slug: "missing_index" },
  { pattern: /\bunused index(?:es)?\b/gi, slug: "unused_index" },
  { pattern: /\bduplicate index(?:es)?\b/gi, slug: "duplicate_index" },
  { pattern: /\bclustered index\b/gi, slug: "clustered_index" },
  { pattern: /\bRCSI\b/g, slug: "rcsi" },
  { pattern: /\bMAXDOP\b/gi, slug: "maxdop" },
  { pattern: /\bQuery Store\b/gi, slug: "query_store" },
  { pattern: /\bheaps?\b/gi, slug: "heap" },
  { pattern: /\bDMVs?\b/g, slug: "dmv" },
];

/**
 * Render a plain string, auto-wrapping any recognised jargon in a <Term> so it
 * gets a hover definition. Non-matching text passes through verbatim. Matching
 * is greedy left-to-right and non-overlapping; the first JARGON entry to claim
 * a span wins. Safe to use on any user-facing copy.
 */
export function TermText({ children }: { children: string }) {
  const text = children;
  // Collect non-overlapping matches across all patterns, then stitch.
  type Hit = { start: number; end: number; slug: string };
  const hits: Hit[] = [];
  for (const { pattern, slug } of JARGON) {
    pattern.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = pattern.exec(text)) !== null) {
      const start = m.index;
      const end = start + m[0].length;
      // Skip if this span overlaps one already claimed by an earlier pattern.
      if (hits.some((h) => start < h.end && end > h.start)) continue;
      hits.push({ start, end, slug });
    }
  }
  if (hits.length === 0) return <>{text}</>;
  hits.sort((a, b) => a.start - b.start);

  const out: ReactNode[] = [];
  let cursor = 0;
  hits.forEach((h, i) => {
    if (h.start < cursor) return; // defensive: dropped by overlap above
    if (h.start > cursor) out.push(text.slice(cursor, h.start));
    out.push(
      <Term key={`t${i}`} k={h.slug}>
        {text.slice(h.start, h.end)}
      </Term>,
    );
    cursor = h.end;
  });
  if (cursor < text.length) out.push(text.slice(cursor));
  return <>{out}</>;
}

/**
 * Inline glossary tooltip. Wraps any jargon in plain text:
 *
 *   <Term k="sargable">non-SARGable</Term>
 *
 * Renders the children with a dotted underline + cursor:help; on hover or
 * keyboard focus it pops a small definition card (GLOSSARY[k].short, plus a
 * "Learn more" link when docUrl is set). Keyboard accessible: the trigger is
 * focusable and the popover is wired via aria-describedby.
 *
 * The popover is positioned with `position: fixed` measured off the trigger's
 * bounding box, so it never gets clipped by an `overflow:hidden` ancestor
 * (every workspace body clips). Unknown keys render the children plain — adding
 * <Term> around text is always safe.
 */
export function Term({
  k,
  children,
  className,
}: {
  k: string;
  children: ReactNode;
  className?: string;
}) {
  const entry = GLOSSARY[k];
  const id = useId();
  const triggerRef = useRef<HTMLSpanElement>(null);
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ left: number; top: number; below: boolean }>({
    left: 0,
    top: 0,
    below: false,
  });

  // Measure the trigger and decide whether the popover sits above (default) or
  // below (if there isn't room above). Width is capped in CSS (.term-pop).
  // NB: declared before the early return so hook order stays stable.
  useLayoutEffect(() => {
    if (!open || !triggerRef.current) return;
    const r = triggerRef.current.getBoundingClientRect();
    const POP_W = 300;
    const ESTIMATED_H = 130;
    const below = r.top < ESTIMATED_H + 16;
    let left = r.left + r.width / 2 - POP_W / 2;
    left = Math.max(10, Math.min(left, window.innerWidth - POP_W - 10));
    const top = below ? r.bottom + 8 : r.top - 8;
    setPos({ left, top, below });
  }, [open]);

  // Unknown term → render the children verbatim, no decoration.
  if (!entry) return <>{children}</>;

  return (
    <span
      ref={triggerRef}
      className={`term${className ? ` ${className}` : ""}`}
      tabIndex={0}
      role="button"
      aria-describedby={open ? id : undefined}
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      onFocus={() => setOpen(true)}
      onBlur={() => setOpen(false)}
      onKeyDown={(e) => {
        if (e.key === "Escape") setOpen(false);
      }}
    >
      {children}
      {open && (
        <span
          id={id}
          role="tooltip"
          className={`term-pop${pos.below ? " below" : ""}`}
          style={{
            left: pos.left,
            top: pos.top,
            transform: pos.below ? "none" : "translateY(-100%)",
          }}
          // Stop the wrapper's mouseleave from firing while the cursor is over
          // the popover itself (so "Learn more" stays clickable).
          onMouseEnter={() => setOpen(true)}
        >
          <span className="term-pop-name">{entry.term}</span>
          <span className="term-pop-def">{entry.short}</span>
          {entry.docUrl && (
            <a
              className="term-pop-link"
              href={entry.docUrl}
              target="_blank"
              rel="noopener noreferrer"
              // Keep the trigger "focused" enough not to dismiss before click.
              onMouseDown={(e) => e.preventDefault()}
            >
              Learn more ↗
            </a>
          )}
        </span>
      )}
    </span>
  );
}
