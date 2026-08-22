/**
 * "Counters since <date> (N h)" — the age of the DMV usage counters behind the
 * advisor / health verdicts. SQL Server resets sys.dm_db_index_usage_stats and
 * friends on restart, so a verdict like "0 updates" or "runs ~0.3×/day" drawn
 * from one-hour-old counters is provisional, not measured history. Under 24h
 * the chip turns amber and says so; with the fields absent (older backend) it
 * renders nothing rather than guessing.
 */
export function CounterAgeChip({
  ageSecs,
  since,
}: {
  ageSecs?: number | null;
  since?: string | null;
}) {
  if (ageSecs == null && !since) return null;
  const age = ageSecs ?? (since ? Math.max(0, (Date.now() - Date.parse(since)) / 1000) : null);
  if (age == null || !Number.isFinite(age)) return null;
  const young = age < 24 * 3600;
  const sinceText = since ? fmtSince(since) : null;
  const title = young
    ? `The usage counters this advice is built on were reset ${fmtAge(age)} ago (SQL Server restart or database state change). Under 24 hours they have not seen a full daily cycle — treat "unused", "never seeks" and ×/day figures as provisional until they age.`
    : `Usage counters have accumulated for ${fmtAge(age)}${sinceText ? ` (since ${sinceText})` : ""} — a full daily cycle or more, so usage-based advice reflects real history.`;
  return (
    <span className={`counter-age${young ? " young" : ""}`} title={title} data-testid="counter-age">
      <span className="counter-age-glyph" aria-hidden>{young ? "⚠" : "◷"}</span>
      <span className="counter-age-text">
        Counters since {sinceText ?? "reset"} ({fmtAge(age)})
      </span>
      {young && <span className="counter-age-note">young counters — advice is provisional</span>}
    </span>
  );
}

function fmtAge(secs: number): string {
  if (secs < 3600) return `${Math.max(1, Math.round(secs / 60))} min`;
  const h = secs / 3600;
  if (h < 48) return `${h < 10 ? h.toFixed(1) : Math.round(h)} h`;
  return `${Math.round(h / 24)} d`;
}

function fmtSince(iso: string): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return iso;
  return new Date(t).toLocaleString(undefined, {
    month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
  });
}
