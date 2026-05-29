/**
 * Empty-state card for charts and panels. Optionally renders a single
 * primary CTA so an empty chart is never a dead end — the caller passes
 * `action` to route the user somewhere useful (connect, load a plan, …).
 *
 * Pass 5 A1: a chart can now PULL its own DMV bundle in place rather than
 * routing the user back to CONN. `loading` swaps the card into a "Pulling
 * DMVs…" spinner state; `error` shows an inline failure with the same
 * `action` button re-labelled "Retry" (no redirect). The action itself does
 * the in-place pull (App.pullDmvInline), so the chart fills without leaving.
 */
export function EmptyChart({
  glyph,
  title,
  hint,
  action,
  loading,
  error,
}: {
  glyph: string;
  title: string;
  hint: string;
  action?: { label: string; onClick: () => void };
  /** True while the in-place DMV pull is in flight → shows the spinner state. */
  loading?: boolean;
  /** Inline error from the last pull attempt — keeps the user in place to retry. */
  error?: string | null;
}) {
  if (loading) {
    return (
      <div className="empty">
        <div className="empty-card empty-loading">
          <div className="empty-spinner" aria-hidden />
          <div className="empty-title">Pulling DMVs…</div>
          <div className="empty-hint">
            Reading live performance views from the connected server. This can take a few seconds.
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="empty">
      <div className="empty-card">
        <div className="empty-glyph">{glyph}</div>
        <div className="empty-title">{title}</div>
        <div className="empty-hint">{hint}</div>
        {error && <div className="empty-error">{error}</div>}
        {action && (
          <div className="empty-action">
            <button className="btn primary" onClick={action.onClick}>
              {error ? "Retry" : action.label}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
