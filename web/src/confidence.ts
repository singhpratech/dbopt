import type { Confidence } from "./api/backend";

/**
 * ONE confidence vocabulary, shared by every surface (metric chips, the
 * HealthOverview signal strip, issue cards, IssueDetailPane, ADVISE rec cards).
 *
 * The glyph is the glanceable trust signal — readable without a hover — and the
 * color tier matches the existing `.conf-*` CSS already used by MetricChip's
 * popover and the confidence badge:
 *   • observed  = ✓  (signal color)  — measured directly from DMV counters.
 *   • estimated = ○  (info color)    — SQL Server's own projection.
 *   • heuristic = ⚡ (warn color)    — a rule-of-thumb; verify before acting.
 *
 * Keep this the single source of truth: Phase B and any future surface should
 * import CONF_GLYPH / confGlyph rather than re-hardcoding the symbols.
 */
export const CONF_GLYPH: Record<Confidence, string> = {
  observed: "✓",
  estimated: "○",
  heuristic: "⚡",
};

/** Short tier word for a label next to the glyph ("observed" / "estimated" / …). */
export const CONF_LABEL: Record<Confidence, string> = {
  observed: "observed",
  estimated: "estimated",
  heuristic: "heuristic",
};

/**
 * Glyph for any confidence value (tolerant of a raw backend string / missing
 * value — falls back to the observed tick so an unknown band never blanks out).
 */
export function confGlyph(c?: string): string {
  return CONF_GLYPH[(c as Confidence) ?? "observed"] ?? CONF_GLYPH.observed;
}

/** Normalize an optional/raw confidence string to a known Confidence tier. */
export function confTier(c?: string): Confidence {
  return c === "estimated" || c === "heuristic" ? c : "observed";
}

/**
 * One-line tooltip explaining the tier — used as a `title=` on the glyph so a
 * hover still teaches even where there's no <Term> popover.
 */
export function confTitle(c?: string): string {
  switch (confTier(c)) {
    case "observed":
      return "Observed — measured directly from live DMV counters.";
    case "estimated":
      return "Estimated — SQL Server's own projection, not a measured outcome.";
    case "heuristic":
      return "Heuristic — a rule-of-thumb; verify before acting.";
  }
}
