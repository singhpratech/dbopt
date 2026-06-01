//! Write-cost estimation + workload grounding for the CONNECTED index advisor.
//!
//! This module is the layer that lets our advisor BEAT a naive missing-index
//! reporter (the kind the community index-health script surfaces): instead of blindly echoing every
//! suggestion SQL Server's DMV emits, we
//!
//!   (a) estimate the *relative write cost* of each candidate index from its
//!       key + INCLUDE width, surface a low / medium / high label, and rank
//!       candidates by **benefit ÷ write-cost** so a narrow high-benefit index
//!       beats a sprawling 12-column one with the same raw DMV score; and
//!   (b) tie each candidate to **how often the benefiting query runs** when
//!       Query-Store / exec-count workload data is present in the bundle, so a
//!       rec can read "helps a query that runs N×/day" and hot queries float to
//!       the top. When no workload data is present we degrade gracefully and
//!       fall back to the DMV's own seek counts.
//!
//! Everything here is a pure function over plain data so it unit-tests in
//! isolation and never touches the database. It is ADDITIVE: `dmv::advise()`
//! calls into it to enrich the existing `CreateIndex` recommendations; nothing
//! in the existing pipeline is removed or rewritten.

use crate::dmv::MissingIndex;
use serde::{Deserialize, Serialize};

/// Observed execution frequency for a query whose plan requested a missing
/// index, sourced from Query Store / `sys.dm_exec_query_stats` when available.
///
/// We deliberately key this to a (schema, table) the query touches rather than
/// to an opaque query hash, because the missing-index DMV groups by the target
/// object, not by query. This is a pragmatic join: a candidate index on a table
/// is credited with the busiest query observed against that table. It is a
/// heuristic, and we label it as such in the rec.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryWorkloadStat {
    pub schema_name: String,
    pub table_name: String,
    /// Total times the query executed over the capture window.
    #[serde(default)]
    pub execution_count: u64,
    /// Length of the capture window in hours (so we can normalise to per-day).
    /// Defaults to 24h when absent so a single day's capture reads naturally.
    #[serde(default)]
    pub window_hours: f64,
}

impl QueryWorkloadStat {
    /// Executions normalised to a per-day rate. Degrades to the raw count when
    /// the window is unknown / zero (treated as a single 24h day).
    pub fn executions_per_day(&self) -> f64 {
        if self.window_hours <= 0.0 {
            self.execution_count as f64
        } else {
            (self.execution_count as f64) * 24.0 / self.window_hours
        }
    }
}

/// Relative write-cost tier of a candidate index. The wider the index (more key
/// columns + more INCLUDE columns), the more every INSERT/UPDATE/DELETE on the
/// base table has to maintain, and the more storage + log it burns. We do NOT
/// claim an absolute cost (we have no row-width bytes here) — this is a relative
/// ranking signal, surfaced as a human label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteCost {
    Low,
    Medium,
    High,
}

impl WriteCost {
    pub fn label(self) -> &'static str {
        match self {
            WriteCost::Low => "low",
            WriteCost::Medium => "medium",
            WriteCost::High => "high",
        }
    }
    /// Divisor used to down-rank wide indexes. Benefit is divided by this, so a
    /// high-write-cost index needs proportionally more benefit to outrank a
    /// narrow one. Tuned so a 1-key/0-include index is unpenalised (1.0) while a
    /// sprawling index is penalised ~3×.
    pub fn rank_divisor(self) -> f64 {
        match self {
            WriteCost::Low => 1.0,
            WriteCost::Medium => 1.75,
            WriteCost::High => 3.0,
        }
    }
}

/// A "column width units" estimate for a candidate index. Key columns cost more
/// than INCLUDE columns to maintain (they participate in B-tree ordering and
/// every nonclustered index also carries the clustered key as a locator), so we
/// weight keys 2× includes. Pure integer arithmetic, no DB access.
pub fn estimate_width_units(key_columns: usize, include_columns: usize) -> usize {
    key_columns * 2 + include_columns
}

/// Map a candidate's key + INCLUDE shape to a relative write-cost tier.
///
/// Thresholds are deliberately conservative so the common, healthy
/// "2-3 key columns, a couple of includes" index stays `low`/`medium` and only
/// genuinely sprawling indexes (the ones that quietly tax every write) get the
/// `high` flag that down-ranks them.
pub fn classify_write_cost(key_columns: usize, include_columns: usize) -> WriteCost {
    let units = estimate_width_units(key_columns, include_columns);
    // A very wide key is the strongest signal on its own: more than 4 key
    // columns means every write reshuffles a fat B-tree row.
    if key_columns >= 5 || units >= 12 {
        WriteCost::High
    } else if key_columns >= 3 || units >= 6 {
        WriteCost::Medium
    } else {
        WriteCost::Low
    }
}

/// Convenience: classify directly from a `MissingIndex` candidate's shape.
pub fn write_cost_of(m: &MissingIndex) -> WriteCost {
    let keys = m.equality_columns.len() + m.inequality_columns.len();
    classify_write_cost(keys, m.included_columns.len())
}

/// Find the busiest workload entry for a (schema, table), case-insensitively.
/// Returns `None` when no workload data covers the table (graceful degrade).
pub fn busiest_for<'a>(
    workload: &'a [QueryWorkloadStat],
    schema: &str,
    table: &str,
) -> Option<&'a QueryWorkloadStat> {
    workload
        .iter()
        .filter(|w| {
            w.schema_name.eq_ignore_ascii_case(schema)
                && w.table_name.eq_ignore_ascii_case(table)
        })
        .max_by(|a, b| {
            a.executions_per_day()
                .partial_cmp(&b.executions_per_day())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// The final write-cost-and-workload-adjusted ranking score for a CreateIndex
/// candidate, plus the human-facing pieces the advisor surfaces.
#[derive(Debug, Clone)]
pub struct CandidateRanking {
    /// benefit ÷ write-cost, further boosted by observed query frequency.
    pub adjusted_score: f64,
    pub write_cost: WriteCost,
    /// `Some(per-day rate)` when workload data covered this table.
    pub executions_per_day: Option<f64>,
}

/// Compute the adjusted ranking for a candidate.
///
/// * `base_score` is SQL Server's own improvement measure (cost × impact ×
///   seeks) that `advise()` already computes — we take it as the raw benefit.
/// * We divide by the write-cost divisor so wide indexes need more benefit to
///   rank high (this is the "BEAT the community index-health script" lever: it won't recommend a
///   12-column index just because the DMV screamed for it).
/// * When workload data exists, we multiply by a gentle log-scaled frequency
///   factor so a query that runs 50k×/day outranks one that runs 5×/day without
///   letting frequency completely dominate the cost evidence.
pub fn rank_candidate(
    m: &MissingIndex,
    base_score: f64,
    workload: &[QueryWorkloadStat],
) -> CandidateRanking {
    let write_cost = write_cost_of(m);
    let mut score = base_score / write_cost.rank_divisor();

    let busiest = busiest_for(workload, &m.schema_name, &m.table_name);
    let executions_per_day = busiest.map(|w| w.executions_per_day());
    if let Some(per_day) = executions_per_day {
        // Gentle, saturating boost: 1 + ln(1 + per_day)/ln(10). At 0/day the
        // factor is 1.0 (no effect); ~10/day → ~2×; ~1000/day → ~4×. Hot queries
        // float up without a single mega-query swamping everything.
        let factor = 1.0 + (1.0 + per_day).ln() / std::f64::consts::LN_10;
        score *= factor;
    }

    CandidateRanking {
        adjusted_score: score,
        write_cost,
        executions_per_day,
    }
}

/// Human sentence describing the workload tie-in for a rec, or `None` when no
/// workload data covered the table (so the caller can omit the clause entirely
/// rather than print a misleading "0×/day").
pub fn workload_phrase(executions_per_day: Option<f64>) -> Option<String> {
    let per_day = executions_per_day?;
    if per_day <= 0.0 {
        return None;
    }
    // Round to a readable integer for the common case; keep one decimal for the
    // sub-1/day long tail so we never print "helps a query that runs 0×/day".
    let rendered = if per_day >= 1.0 {
        format!("{:.0}", per_day.round())
    } else {
        format!("{per_day:.1}")
    };
    Some(format!("helps a query that runs ~{rendered}×/day"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mi(eq: &[&str], ineq: &[&str], inc: &[&str]) -> MissingIndex {
        MissingIndex {
            schema_name: "dbo".into(),
            table_name: "Orders".into(),
            equality_columns: eq.iter().map(|s| s.to_string()).collect(),
            inequality_columns: ineq.iter().map(|s| s.to_string()).collect(),
            included_columns: inc.iter().map(|s| s.to_string()).collect(),
            avg_user_impact: 90.0,
            user_seeks: 100,
            avg_total_user_cost: 10.0,
        }
    }

    #[test]
    fn write_cost_tiers_scale_with_width() {
        // Narrow index → low.
        assert_eq!(classify_write_cost(1, 0), WriteCost::Low);
        assert_eq!(classify_write_cost(2, 1), WriteCost::Low);
        // Three keys or moderate width → medium.
        assert_eq!(classify_write_cost(3, 0), WriteCost::Medium);
        assert_eq!(classify_write_cost(2, 3), WriteCost::Medium);
        // Sprawling → high.
        assert_eq!(classify_write_cost(5, 0), WriteCost::High);
        assert_eq!(classify_write_cost(2, 9), WriteCost::High);
    }

    #[test]
    fn wide_index_is_down_ranked_vs_narrow_at_equal_benefit() {
        // Same raw DMV benefit; the narrow candidate must out-rank the wide one
        // purely because it taxes writes less. This is the core "beat the naive
        // reporter" behaviour.
        let narrow = mi(&["CustomerId"], &[], &[]);
        let wide = mi(
            &["A", "B", "C", "D", "E"],
            &[],
            &["X", "Y", "Z", "W"],
        );
        let base = 100.0;
        let rn = rank_candidate(&narrow, base, &[]);
        let rw = rank_candidate(&wide, base, &[]);
        assert_eq!(rn.write_cost, WriteCost::Low);
        assert_eq!(rw.write_cost, WriteCost::High);
        assert!(
            rn.adjusted_score > rw.adjusted_score,
            "narrow {} should beat wide {}",
            rn.adjusted_score,
            rw.adjusted_score
        );
    }

    #[test]
    fn hot_query_outranks_cold_at_equal_shape_and_benefit() {
        // Identical candidate shape + base score; the one whose table sees a
        // hot query must rank higher.
        let cand = mi(&["CustomerId"], &[], &[]);
        let cold = vec![QueryWorkloadStat {
            schema_name: "dbo".into(),
            table_name: "Orders".into(),
            execution_count: 5,
            window_hours: 24.0,
        }];
        let hot = vec![QueryWorkloadStat {
            schema_name: "dbo".into(),
            table_name: "Orders".into(),
            execution_count: 50_000,
            window_hours: 24.0,
        }];
        let rc = rank_candidate(&cand, 10.0, &cold);
        let rh = rank_candidate(&cand, 10.0, &hot);
        assert!(rh.adjusted_score > rc.adjusted_score);
        assert!(rh.executions_per_day.unwrap() > 49_000.0);
    }

    #[test]
    fn missing_workload_degrades_gracefully() {
        // No workload data → no frequency boost, no phrase, but still a valid
        // (write-cost-only) ranking.
        let cand = mi(&["CustomerId"], &[], &[]);
        let r = rank_candidate(&cand, 42.0, &[]);
        assert_eq!(r.executions_per_day, None);
        assert!((r.adjusted_score - 42.0).abs() < 1e-9);
        assert!(workload_phrase(r.executions_per_day).is_none());
    }

    #[test]
    fn workload_phrase_reads_naturally() {
        assert_eq!(
            workload_phrase(Some(1200.0)).as_deref(),
            Some("helps a query that runs ~1200×/day")
        );
        // Sub-daily long tail keeps a decimal instead of rounding to 0.
        assert_eq!(
            workload_phrase(Some(0.5)).as_deref(),
            Some("helps a query that runs ~0.5×/day")
        );
        // Zero / negative is omitted rather than printed misleadingly.
        assert!(workload_phrase(Some(0.0)).is_none());
        assert!(workload_phrase(None).is_none());
    }

    #[test]
    fn executions_per_day_normalises_window() {
        // 1000 execs over a 48h window = 500/day.
        let w = QueryWorkloadStat {
            schema_name: "dbo".into(),
            table_name: "Orders".into(),
            execution_count: 1000,
            window_hours: 48.0,
        };
        assert!((w.executions_per_day() - 500.0).abs() < 1e-9);
        // Unknown window → treat raw count as one day's worth.
        let w0 = QueryWorkloadStat {
            schema_name: "dbo".into(),
            table_name: "Orders".into(),
            execution_count: 1000,
            window_hours: 0.0,
        };
        assert!((w0.executions_per_day() - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn busiest_for_picks_hottest_and_is_case_insensitive() {
        let workload = vec![
            QueryWorkloadStat { schema_name: "dbo".into(), table_name: "Orders".into(), execution_count: 10, window_hours: 24.0 },
            QueryWorkloadStat { schema_name: "DBO".into(), table_name: "ORDERS".into(), execution_count: 9999, window_hours: 24.0 },
            QueryWorkloadStat { schema_name: "dbo".into(), table_name: "Other".into(), execution_count: 1_000_000, window_hours: 24.0 },
        ];
        let b = busiest_for(&workload, "dbo", "Orders").unwrap();
        assert_eq!(b.execution_count, 9999);
        assert!(busiest_for(&workload, "dbo", "Nope").is_none());
    }
}
