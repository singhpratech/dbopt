//! Weekly report generator. Pulls aggregates out of the sentinel store
//! and renders them as Markdown / HTML.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

use crate::storage::{
    PainSummary, RegressionRow, Storage, TimeRange, TopQueryRow, UnusedIndexRow,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyReport {
    pub window_from: chrono::DateTime<chrono::Utc>,
    pub window_to: chrono::DateTime<chrono::Utc>,
    pub instances: i64,
    pub pain: PainSummaryDto,
    pub top_queries: Vec<TopQueryDto>,
    /// Same queries ranked by most-recent execution (the "by last run" view).
    #[serde(default)]
    pub recent_queries: Vec<TopQueryDto>,
    pub regressions: Vec<RegressionDto>,
    pub unused_indexes: Vec<UnusedIndexDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PainSummaryDto {
    pub top_wait_type: Option<String>,
    pub top_wait_time_ms: i64,
    pub deadlock_count: i64,
    pub blocking_incidents: i64,
}

impl From<PainSummary> for PainSummaryDto {
    fn from(p: PainSummary) -> Self {
        Self {
            top_wait_type: p.top_wait_type,
            top_wait_time_ms: p.top_wait_time_ms,
            deadlock_count: p.deadlock_count,
            blocking_incidents: p.blocking_incidents,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopQueryDto {
    pub query_id: i64,
    pub plan_id: i64,
    pub total_duration_ms: i64,
    pub executions: i64,
    pub avg_duration_ms: i64,
    /// The captured T-SQL text (truncated), or null for rows captured by builds
    /// before query-text capture existed.
    #[serde(default)]
    pub query_sql_text: Option<String>,
    /// Most recent execution time (unix ms), or null if unknown. Powers the
    /// "by last run" sort and the LAST RUN column.
    #[serde(default)]
    pub last_run_ms: Option<i64>,
}

impl From<TopQueryRow> for TopQueryDto {
    fn from(r: TopQueryRow) -> Self {
        let avg = if r.executions > 0 { r.total_duration_ms / r.executions } else { 0 };
        Self {
            query_id: r.query_id,
            plan_id: r.plan_id,
            total_duration_ms: r.total_duration_ms,
            executions: r.executions,
            avg_duration_ms: avg,
            query_sql_text: r.query_sql_text,
            last_run_ms: r.last_execution_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionDto {
    pub query_id: i64,
    pub baseline_duration_ms: i64,
    pub current_duration_ms: i64,
    pub delta_pct: f64,
}

impl From<RegressionRow> for RegressionDto {
    fn from(r: RegressionRow) -> Self {
        Self {
            query_id: r.query_id,
            baseline_duration_ms: r.baseline_duration_ms,
            current_duration_ms: r.current_duration_ms,
            delta_pct: r.delta_pct,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnusedIndexDto {
    pub db_name: String,
    pub schema_name: String,
    pub table_name: String,
    pub index_name: String,
    pub updates_in_window: i64,
}

impl From<UnusedIndexRow> for UnusedIndexDto {
    fn from(r: UnusedIndexRow) -> Self {
        Self {
            db_name: r.db_name,
            schema_name: r.schema_name,
            table_name: r.table_name,
            index_name: r.index_name,
            updates_in_window: r.updates_in_window,
        }
    }
}

/// Build a report for the given window.
pub fn render_weekly(storage: &Storage, window: TimeRange) -> WeeklyReport {
    let top_queries = storage
        .top_n_by_duration(window, 25)
        .unwrap_or_default()
        .into_iter()
        .map(TopQueryDto::from)
        .collect();
    let recent_queries = storage
        .top_n_by_recency(window, 25)
        .unwrap_or_default()
        .into_iter()
        .map(TopQueryDto::from)
        .collect();
    let regressions = storage
        .regressions_since(window)
        .unwrap_or_default()
        .into_iter()
        .map(RegressionDto::from)
        .collect();
    let unused_indexes = storage
        .unused_indexes(window)
        .unwrap_or_default()
        .into_iter()
        .map(UnusedIndexDto::from)
        .collect();
    let pain = storage.pain_summary(window).unwrap_or_default().into();
    let instances = storage.instance_count().unwrap_or(0);

    WeeklyReport {
        window_from: window.from,
        window_to: window.to,
        instances,
        pain,
        top_queries,
        recent_queries,
        regressions,
        unused_indexes,
    }
}

fn fmt_ms(ms: i64) -> String {
    if ms >= 60_000 { format!("{:.1} min", ms as f64 / 60_000.0) }
    else if ms >= 1_000 { format!("{:.2} s", ms as f64 / 1_000.0) }
    else { format!("{} ms", ms) }
}

/// Render the report as a Markdown document.
pub fn render_markdown(r: &WeeklyReport) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# sqlopt sentinel · pain report");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "**Window** &nbsp; {} → {}  \n**Instances tracked** &nbsp; {}",
        r.window_from.format("%Y-%m-%d %H:%M UTC"),
        r.window_to.format("%Y-%m-%d %H:%M UTC"),
        r.instances,
    );
    let _ = writeln!(s);

    // ── Pain summary ────────────────────────────────────
    let _ = writeln!(s, "## Headline pain");
    let _ = writeln!(s);
    let _ = writeln!(s, "| Metric | Value |");
    let _ = writeln!(s, "|---|---|");
    let _ = writeln!(
        s,
        "| Top wait | {} ({}) |",
        r.pain.top_wait_type.as_deref().unwrap_or("—"),
        fmt_ms(r.pain.top_wait_time_ms),
    );
    let _ = writeln!(s, "| Deadlocks captured | {} |", r.pain.deadlock_count);
    let _ = writeln!(s, "| Blocking incidents | {} |", r.pain.blocking_incidents);
    let _ = writeln!(s);

    // ── Top queries by total duration ───────────────────
    let _ = writeln!(s, "## Top {} queries by total duration", r.top_queries.len());
    let _ = writeln!(s);
    if r.top_queries.is_empty() {
        let _ = writeln!(s, "_No Query Store rows in window. Enable Query Store on the target database._");
    } else {
        let _ = writeln!(s, "| Query | SQL | Total | Executions | Avg |");
        let _ = writeln!(s, "|---|---|---:|---:|---:|");
        for q in &r.top_queries {
            let sql = q
                .query_sql_text
                .as_deref()
                .map(|t| {
                    let one_line = t.split_whitespace().collect::<Vec<_>>().join(" ");
                    let clipped: String = one_line.chars().take(80).collect();
                    format!("`{}`", clipped.replace('|', "\\|"))
                })
                .unwrap_or_else(|| "—".to_string());
            let _ = writeln!(
                s,
                "| `{}` | {} | {} | {} | {} |",
                q.query_id, sql, fmt_ms(q.total_duration_ms), q.executions, fmt_ms(q.avg_duration_ms),
            );
        }
    }
    let _ = writeln!(s);

    // ── Regressions ─────────────────────────────────────
    let _ = writeln!(s, "## Regressions (≥2× slower vs. baseline half-window)");
    let _ = writeln!(s);
    if r.regressions.is_empty() {
        let _ = writeln!(s, "_No regressions detected._");
    } else {
        let _ = writeln!(s, "| Query | Baseline | Current | Δ |");
        let _ = writeln!(s, "|---|---:|---:|---:|");
        for x in &r.regressions {
            let _ = writeln!(
                s,
                "| `{}` | {} | {} | +{:.0}% |",
                x.query_id,
                fmt_ms(x.baseline_duration_ms),
                fmt_ms(x.current_duration_ms),
                x.delta_pct,
            );
        }
    }
    let _ = writeln!(s);

    // ── Unused indexes ──────────────────────────────────
    let _ = writeln!(s, "## Indexes accumulating writes with zero reads");
    let _ = writeln!(s);
    if r.unused_indexes.is_empty() {
        let _ = writeln!(s, "_No fully-unused indexes in window._");
    } else {
        let _ = writeln!(s, "| Table | Index | Writes |");
        let _ = writeln!(s, "|---|---|---:|");
        for u in &r.unused_indexes {
            let _ = writeln!(
                s,
                "| `{}.{}.{}` | `{}` | {} |",
                u.db_name, u.schema_name, u.table_name, u.index_name, u.updates_in_window,
            );
        }
    }
    let _ = writeln!(s);

    let _ = writeln!(
        s,
        "---\n_Generated by `sqlopt-sentinel`. Each row above can be cross-referenced against the static analyzer findings in the AI workspace._"
    );
    s
}

/// Minimal HTML entity escaping for untrusted text (SQL bodies, object names).
/// Keeps the report XSS-safe and prevents a stray `<` from breaking the markup.
fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            _ => o.push(c),
        }
    }
    o
}

/// Collapse a captured SQL body to a single whitespace-normalized line (em-dash
/// when absent). Caller is responsible for HTML-escaping the result.
fn one_line(t: Option<&str>) -> String {
    match t {
        Some(t) if !t.trim().is_empty() => t.split_whitespace().collect::<Vec<_>>().join(" "),
        _ => "—".to_string(),
    }
}

/// Absolute UTC timestamp for a unix-ms instant (em-dash when unknown). The
/// report is read later, so an absolute time beats a relative "x ago".
fn fmt_ts_ms(ms: Option<i64>) -> String {
    match ms.and_then(|m| chrono::DateTime::<chrono::Utc>::from_timestamp_millis(m)) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M UTC").to_string(),
        None => "—".to_string(),
    }
}

/// Self-contained dark stylesheet for the HTML report — matches the app palette.
const HTML_STYLE: &str = "\
:root{--bg:#0a0d12;--panel:#0e131b;--elev:#131822;--line:#1c2230;--line2:#283042;--text:#d6dbe5;--dim:#9aa3b5;--muted:#6b748a;--accent:#d4ff4e;--ok:#5dd39e;--crit:#ff3a4a}\
*{box-sizing:border-box}\
body{font:13px/1.6 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;background:var(--bg);color:var(--text);margin:0 auto;padding:28px;max-width:1100px}\
.rpt-head{border-bottom:1px solid var(--line2);padding-bottom:14px;margin-bottom:22px}\
.brand{font-size:18px;letter-spacing:.04em}.brand .mark{color:var(--accent)}.brand .dim{color:var(--muted)}\
.meta{color:var(--dim);font-size:12px;margin-top:6px}.meta b{color:var(--text);font-weight:600}\
.cards{display:grid;grid-template-columns:repeat(4,1fr);gap:12px;margin-bottom:26px}\
@media(max-width:720px){.cards{grid-template-columns:repeat(2,1fr)}}\
.card{border:1px solid var(--line2);background:var(--elev);padding:12px 14px;border-radius:5px}\
.card .k{font-size:10px;letter-spacing:.12em;color:var(--muted)}\
.card .v{font-size:17px;color:var(--text);margin-top:5px;word-break:break-word}\
.card .sub{font-size:11px;color:var(--dim);margin-top:3px}\
h2{color:var(--accent);font-weight:500;font-size:14px;letter-spacing:.06em;border-bottom:1px solid var(--line);padding-bottom:7px;margin:30px 0 10px}\
table{border-collapse:collapse;width:100%;margin:6px 0 8px}\
th,td{border-bottom:1px solid var(--line);padding:7px 10px;text-align:left;vertical-align:top}\
th{font-size:10px;color:var(--muted);font-weight:500;letter-spacing:.1em;text-transform:uppercase}\
td{font-size:12px}\
.num{text-align:right;white-space:nowrap}.dim{color:var(--dim)}.crit{color:var(--crit);font-weight:600}.mono{color:var(--dim)}\
td.sql{max-width:540px}\
td.sql code{display:block;background:var(--panel);border:1px solid var(--line);border-radius:3px;padding:5px 8px;color:var(--accent);font-size:11.5px;white-space:pre-wrap;word-break:break-word;max-height:150px;overflow:auto}\
.bar{height:4px;background:var(--line);border-radius:2px;overflow:hidden;margin-bottom:3px}\
.bar span{display:block;height:100%;background:var(--accent)}\
.empty{color:var(--dim);font-style:italic;padding:4px 0 10px}\
footer{margin-top:30px;padding-top:14px;border-top:1px solid var(--line);color:var(--muted);font-size:11px}";

/// Render the report as a self-contained, styled HTML document built directly
/// from the report data (NOT by string-munging the markdown — that left tables
/// and headings as literal text). Inline CSS + inline duration bars, no JS, no
/// external assets: open in any browser, or download & email.
pub fn render_html(r: &WeeklyReport) -> String {
    let generated = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
    let mut body = String::new();

    // ── Header ──────────────────────────────────────────
    let _ = write!(
        body,
        "<header class=\"rpt-head\"><div class=\"brand\"><span class=\"mark\">▣</span> dbopt <span class=\"dim\">/ sentinel pain report</span></div>\
<div class=\"meta\">window <b>{}</b> → <b>{}</b> · {} instance(s) · generated {}</div></header>",
        r.window_from.format("%Y-%m-%d %H:%M UTC"),
        r.window_to.format("%Y-%m-%d %H:%M UTC"),
        r.instances,
        generated,
    );

    // ── Summary cards ───────────────────────────────────
    let _ = write!(
        body,
        "<section class=\"cards\">\
<div class=\"card\"><div class=\"k\">TOP WAIT</div><div class=\"v\">{}</div><div class=\"sub\">{}</div></div>\
<div class=\"card\"><div class=\"k\">DEADLOCKS</div><div class=\"v\">{}</div></div>\
<div class=\"card\"><div class=\"k\">BLOCKING INCIDENTS</div><div class=\"v\">{}</div></div>\
<div class=\"card\"><div class=\"k\">INSTANCES</div><div class=\"v\">{}</div></div>\
</section>",
        esc(r.pain.top_wait_type.as_deref().unwrap_or("—")),
        fmt_ms(r.pain.top_wait_time_ms),
        r.pain.deadlock_count,
        r.pain.blocking_incidents,
        r.instances,
    );

    // ── Top queries by total duration (with inline proportion bars) ──
    let max_total = r.top_queries.iter().map(|q| q.total_duration_ms).max().unwrap_or(0);
    let _ = write!(body, "<section><h2>Top queries · by total duration</h2>");
    if r.top_queries.is_empty() {
        body.push_str("<p class=\"empty\">No Query Store rows in window. Enable Query Store on the target database, then wait for the next poll.</p>");
    } else {
        body.push_str("<table><thead><tr><th>Query</th><th>SQL text</th><th class=\"num\">Total</th><th class=\"num\">Executions</th><th class=\"num\">Avg</th><th class=\"num\">Last run</th></tr></thead><tbody>");
        for q in &r.top_queries {
            let pct = if max_total > 0 {
                (q.total_duration_ms as f64 / max_total as f64 * 100.0).round() as i64
            } else {
                0
            };
            let _ = write!(
                body,
                "<tr><td class=\"mono\">{}</td><td class=\"sql\"><code>{}</code></td>\
<td class=\"num\"><div class=\"bar\"><span style=\"width:{}%\"></span></div>{}</td>\
<td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num dim\">{}</td></tr>",
                q.query_id,
                esc(&one_line(q.query_sql_text.as_deref())),
                pct,
                fmt_ms(q.total_duration_ms),
                q.executions,
                fmt_ms(q.avg_duration_ms),
                fmt_ts_ms(q.last_run_ms),
            );
        }
        body.push_str("</tbody></table>");
    }
    body.push_str("</section>");

    // ── Top queries by last run ─────────────────────────
    if !r.recent_queries.is_empty() {
        body.push_str("<section><h2>Top queries · by last run</h2><table><thead><tr><th class=\"num\">Last run</th><th>Query</th><th>SQL text</th><th class=\"num\">Total</th><th class=\"num\">Executions</th><th class=\"num\">Avg</th></tr></thead><tbody>");
        for q in &r.recent_queries {
            let _ = write!(
                body,
                "<tr><td class=\"num dim\">{}</td><td class=\"mono\">{}</td><td class=\"sql\"><code>{}</code></td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                fmt_ts_ms(q.last_run_ms),
                q.query_id,
                esc(&one_line(q.query_sql_text.as_deref())),
                fmt_ms(q.total_duration_ms),
                q.executions,
                fmt_ms(q.avg_duration_ms),
            );
        }
        body.push_str("</tbody></table></section>");
    }

    // ── Regressions ─────────────────────────────────────
    body.push_str("<section><h2>Regressions · ≥2× slower vs. baseline half-window</h2>");
    if r.regressions.is_empty() {
        body.push_str("<p class=\"empty\">No regressions detected in window.</p>");
    } else {
        body.push_str("<table><thead><tr><th>Query</th><th class=\"num\">Baseline</th><th class=\"num\">Current</th><th class=\"num\">Δ</th></tr></thead><tbody>");
        for x in &r.regressions {
            let _ = write!(
                body,
                "<tr><td class=\"mono\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num crit\">+{:.0}%</td></tr>",
                x.query_id,
                fmt_ms(x.baseline_duration_ms),
                fmt_ms(x.current_duration_ms),
                x.delta_pct,
            );
        }
        body.push_str("</tbody></table>");
    }
    body.push_str("</section>");

    // ── Unused indexes ──────────────────────────────────
    body.push_str("<section><h2>Indexes accumulating writes with zero reads</h2>");
    if r.unused_indexes.is_empty() {
        body.push_str("<p class=\"empty\">No fully-unused indexes in window.</p>");
    } else {
        body.push_str("<table><thead><tr><th>Table</th><th>Index</th><th class=\"num\">Writes</th></tr></thead><tbody>");
        for u in &r.unused_indexes {
            let _ = write!(
                body,
                "<tr><td class=\"mono\">{}</td><td class=\"mono\">{}</td><td class=\"num\">{}</td></tr>",
                esc(&format!("{}.{}.{}", u.db_name, u.schema_name, u.table_name)),
                esc(&u.index_name),
                u.updates_in_window,
            );
        }
        body.push_str("</tbody></table>");
    }
    body.push_str("</section>");

    body.push_str("<footer>Generated by dbopt-sentinel · local-only telemetry. Cross-reference any row against the static analyzer findings in the app.</footer>");

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>dbopt sentinel · pain report</title><style>{}</style></head><body>{}</body></html>",
        HTML_STYLE, body
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    #[test]
    fn renders_empty_report() {
        let s = Storage::open_in_memory().unwrap();
        let report = render_weekly(&s, TimeRange::last_days(7));
        let md = render_markdown(&report);
        assert!(md.contains("# sqlopt sentinel"));
        assert!(md.contains("Top 0 queries"));
        let html = render_html(&report);
        assert!(html.contains("<html"));
        // Real HTML structure, not the old markdown-string munge.
        assert!(html.contains("DEADLOCKS"));
        assert!(html.contains("<section class=\"cards\">"));
        assert!(!html.contains("</p><p>"));
        assert!(!html.contains("## "));
    }
}
