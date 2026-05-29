/**
 * Empty-state card for charts and panels. Optionally renders a single
 * primary CTA so an empty chart is never a dead end — the caller passes
 * `action` to route the user somewhere useful (connect, load a plan, …).
 */
export function EmptyChart({
  glyph,
  title,
  hint,
  action,
}: {
  glyph: string;
  title: string;
  hint: string;
  action?: { label: string; onClick: () => void };
}) {
  return (
    <div className="empty">
      <div className="empty-card">
        <div className="empty-glyph">{glyph}</div>
        <div className="empty-title">{title}</div>
        <div className="empty-hint">{hint}</div>
        {action && (
          <div className="empty-action">
            <button className="btn primary" onClick={action.onClick}>
              {action.label}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
