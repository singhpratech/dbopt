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

/// Render the report as a self-contained HTML document. Reuses the markdown
/// content inside a minimal styled shell so it can be opened in any browser
/// without a markdown engine.
pub fn render_html(r: &WeeklyReport) -> String {
    let md = render_markdown(r);
    // Convert headers/tables into HTML in a very minimal way (no markdown crate
    // — keep deps small). Most consumers will hit the JSON or markdown directly;
    // HTML is the "download and email" path.
    let body = md
        .replace("\n\n", "</p><p>")
        .replace("\n", "<br>");
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>sqlopt sentinel report</title>\
<style>\
body{{font:14px/1.6 ui-monospace,Menlo,monospace;background:#0a0d12;color:#d6dbe5;margin:0;padding:32px;max-width:980px}}\
h1{{color:#d4ff4e;font-weight:500;letter-spacing:0.04em}}\
h2{{color:#d4ff4e;font-weight:500;border-bottom:1px solid #1c2230;padding-bottom:6px;margin-top:32px}}\
table{{border-collapse:collapse;width:100%;margin:8px 0 20px}}\
th,td{{border-bottom:1px solid #1c2230;padding:6px 10px;text-align:left;font-size:12px}}\
th{{color:#6b748a;font-weight:400;letter-spacing:0.1em;text-transform:uppercase}}\
code{{background:#131822;padding:1px 5px;color:#d4ff4e}}\
</style></head><body><p>{}</p></body></html>",
        body
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
        assert!(html.contains("<html>"));
    }
}
