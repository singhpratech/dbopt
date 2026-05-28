export function EmptyChart({ glyph, title, hint }: { glyph: string; title: string; hint: string }) {
  return (
    <div className="empty">
      <div className="empty-card">
        <div className="empty-glyph">{glyph}</div>
        <div className="empty-title">{title}</div>
        <div className="empty-hint">{hint}</div>
      </div>
    </div>
  );
}
