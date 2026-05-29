use crate::findings::{Finding, RuleId, Severity};
use crate::report::{HeatmapCell, SizeNode, ChartData};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DmvBundle {
    #[serde(default)]
    pub index_usage: Vec<IndexUsage>,
    #[serde(default)]
    pub indexes: Vec<IndexMeta>,
    #[serde(default)]
    pub missing_indexes: Vec<MissingIndex>,
    #[serde(default)]
    pub partition_stats: Vec<PartitionStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexUsage {
    pub database_name: String,
    pub schema_name: String,
    pub table_name: String,
    pub index_name: String,
    pub user_seeks: u64,
    pub user_scans: u64,
    pub user_lookups: u64,
    pub user_updates: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMeta {
    pub schema_name: String,
    pub table_name: String,
    pub index_name: String,
    pub is_unique: bool,
    pub is_primary_key: bool,
    pub key_columns: Vec<String>,
    pub included_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingIndex {
    pub schema_name: String,
    pub table_name: String,
    pub equality_columns: Vec<String>,
    pub inequality_columns: Vec<String>,
    pub included_columns: Vec<String>,
    pub avg_user_impact: f64,
    pub user_seeks: u64,
    pub avg_total_user_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionStats {
    pub schema_name: String,
    pub table_name: String,
    pub index_name: Option<String>,
    pub row_count: u64,
    pub reserved_kb: u64,
    pub used_kb: u64,
    pub data_kb: u64,
}

pub struct Advice {
    pub findings: Vec<Finding>,
    pub index_heatmap: Vec<HeatmapCell>,
    pub size_treemap: Vec<SizeNode>,
}

pub fn analyze(bundle: &DmvBundle) -> Advice {
    let mut findings = Vec::new();
    let mut index_heatmap = Vec::new();
    let mut size_treemap = Vec::new();

    // Heatmap: (table, index) → seek/scan/lookup totals
    for u in &bundle.index_usage {
        let table = format!("{}.{}", u.schema_name, u.table_name);
        let total_reads = u.user_seeks + u.user_scans + u.user_lookups;
        index_heatmap.push(HeatmapCell {
            row: table.clone(),
            col: u.index_name.clone(),
            seeks: u.user_seeks,
            scans: u.user_scans,
            lookups: u.user_lookups,
            updates: u.user_updates,
            score: total_reads as i64 - u.user_updates as i64,
        });
        // Unused index: many writes, no reads, and not the PK/clustered
        if total_reads == 0 && u.user_updates > 100 && !u.index_name.eq_ignore_ascii_case("PK") {
            findings.push(Finding {
                rule: RuleId("dmv.unused_index".into()),
                severity: Severity::Warning,
                message: format!(
                    "{}.{} on {} has {} updates and 0 reads since last stats reset — pure write tax.",
                    u.schema_name, u.index_name, table, u.user_updates
                ),
                location: None,
                recommendation: Some("Verify the index is unused across a representative period (a server restart resets DMV counters). If confirmed, DROP it — writes amplify into every nonclustered index.".into()),
            });
        }
    }

    // Duplicate indexes: same leading key columns on same table
    let mut by_table: BTreeMap<(String, String), Vec<&IndexMeta>> = BTreeMap::new();
    for ix in &bundle.indexes {
        by_table.entry((ix.schema_name.clone(), ix.table_name.clone())).or_default().push(ix);
    }
    for ((schema, table), list) in &by_table {
        for i in 0..list.len() {
            for j in (i + 1)..list.len() {
                let a = &list[i];
                let b = &list[j];
                if a.key_columns.is_empty() || b.key_columns.is_empty() { continue; }
                if a.key_columns[0].eq_ignore_ascii_case(&b.key_columns[0]) {
                    // partial or full dup
                    let same = a.key_columns.len() == b.key_columns.len()
                        && a.key_columns.iter().zip(&b.key_columns).all(|(x, y)| x.eq_ignore_ascii_case(y));
                    let sev = if same { Severity::Error } else { Severity::Info };
                    findings.push(Finding {
                        rule: RuleId("dmv.duplicate_or_overlapping_index".into()),
                        severity: sev,
                        message: format!(
                            "Indexes {} and {} on {}.{} share leading key column `{}`. {}",
                            a.index_name, b.index_name, schema, table, a.key_columns[0],
                            if same { "Keys are identical — one of them is pure overhead." } else { "Partial overlap — review whether one can be dropped or merged with INCLUDE." }
                        ),
                        location: None,
                        recommendation: Some("Compare INCLUDE columns and uniqueness. Keep the unique/PK one, merge needed includes into a single covering index, drop the rest.".into()),
                    });
                }
            }
        }
    }

    // Missing indexes (DMV-suggested)
    for m in &bundle.missing_indexes {
        let cols = m.equality_columns.iter().chain(m.inequality_columns.iter()).cloned().collect::<Vec<_>>().join(", ");
        let inc = if m.included_columns.is_empty() { String::new() } else { format!(" INCLUDE ({})", m.included_columns.join(", ")) };
        findings.push(Finding {
            rule: RuleId("dmv.missing_index".into()),
            severity: Severity::Info,
            message: format!(
                "SQL Server's missing-index DMV suggests CREATE INDEX ON {}.{} ({}){} — impact score {:.0}, {} seeks.",
                m.schema_name, m.table_name, cols, inc, m.avg_user_impact, m.user_seeks
            ),
            location: None,
            recommendation: Some("DMV suggestions are heuristics, not commands. Validate against your actual workload, consolidate with existing indexes, and order key columns by selectivity (most selective first).".into()),
        });
    }

    // Size treemap
    for p in &bundle.partition_stats {
        let leaf = p.index_name.clone().unwrap_or_else(|| "(heap)".to_string());
        size_treemap.push(SizeNode {
            schema: p.schema_name.clone(),
            table: p.table_name.clone(),
            index: leaf,
            row_count: p.row_count,
            reserved_kb: p.reserved_kb,
            used_kb: p.used_kb,
            data_kb: p.data_kb,
        });
    }

    // Size finding: largest unused tables (heap with no reads)
    Advice { findings, index_heatmap, size_treemap }
}

#[allow(dead_code)]
pub fn empty_charts() -> ChartData { ChartData::default() }

// ===========================================================================
// Recommendation engine — turns the collected DMV data into ranked,
// copy-paste-ready remediation with the exact T-SQL. This is the prescriptive
// layer: not "here's a metric" but "run this."
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecKind {
    CreateIndex,
    DropIndex,
    MergeIndex,
    ColumnstoreCandidate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    pub kind: RecKind,
    /// "high" | "medium" | "low" — drives sort + UI emphasis.
    pub priority: String,
    pub title: String,
    /// schema.table or schema.table.index
    pub object: String,
    pub rationale: String,
    /// Exact T-SQL to run (already bracket-quoted).
    pub ddl: String,
    /// Numeric impact for ranking within a kind (not comparable across kinds).
    pub impact_score: f64,
    /// Evidence chips (label, value) drawn from the SAME DMV numbers used to
    /// build `rationale`. Lets the health layer render grounded metrics without
    /// re-deriving anything. Additive: defaults to `[]` for any old payload.
    #[serde(default)]
    pub metrics: Vec<(String, String)>,
    /// Provenance of the numbers: `observed` (measured from DMV counters),
    /// `estimated` (SQL Server's own projection), or `heuristic` (rule of
    /// thumb). Additive: defaults to `observed`.
    #[serde(default = "default_confidence")]
    pub confidence: String,
}

fn default_confidence() -> String { "observed".to_string() }

/// Format a kilobyte count as a human MB string with one decimal (e.g. `12.0`).
fn kb_to_mb(kb: u64) -> String { format!("{:.1}", kb as f64 / 1024.0) }
/// Format a kilobyte count as a human GB string with one decimal.
fn kb_to_gb(kb: u64) -> String { format!("{:.1}", kb as f64 / 1_048_576.0) }
/// Thousands-separate an integer count (e.g. `4,200`). No external deps.
fn commas(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in s.chars().enumerate() {
        if i != 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Strip brackets/whitespace from an identifier fragment.
fn unbr(s: &str) -> String {
    s.trim().trim_matches(|c| c == '[' || c == ']').to_string()
}
/// Bracket-quote an identifier.
fn br(s: &str) -> String { format!("[{}]", unbr(s)) }
/// Sanitize an identifier for use inside a generated index name.
fn idfrag(s: &str) -> String {
    unbr(s).chars().filter(|c| c.is_alphanumeric() || *c == '_').collect()
}
fn priority_rank(p: &str) -> u8 { match p { "high" => 0, "medium" => 1, _ => 2 } }

/// Generate ranked, prescriptive recommendations from a DMV bundle.
pub fn advise(bundle: &DmvBundle) -> Vec<Recommendation> {
    let mut recs: Vec<Recommendation> = Vec::new();

    // ---- CreateIndex: from SQL Server's own missing-index DMV --------------
    for m in &bundle.missing_indexes {
        let keys: Vec<String> = m.equality_columns.iter().chain(m.inequality_columns.iter()).cloned().collect();
        if keys.is_empty() { continue; }
        // SQL Server's standard "improvement measure".
        let score = m.avg_total_user_cost * (m.avg_user_impact / 100.0) * (m.user_seeks.max(1) as f64);
        let key_list = keys.iter().map(|c| br(c)).collect::<Vec<_>>().join(", ");
        let inc_list = m.included_columns.iter().map(|c| br(c)).collect::<Vec<_>>().join(", ");
        let inc_clause = if m.included_columns.is_empty() { String::new() } else { format!("\n  INCLUDE ({})", inc_list) };
        let mut name = format!("IX_{}_{}", idfrag(&m.table_name), keys.iter().map(|c| idfrag(c)).collect::<Vec<_>>().join("_"));
        name.truncate(120);
        let ddl = format!(
            "CREATE NONCLUSTERED INDEX [{}]\n  ON {}.{} ({}){};",
            name, br(&m.schema_name), br(&m.table_name), key_list, inc_clause
        );
        let priority = if score >= 10.0 { "high" } else if score >= 1.0 { "medium" } else { "low" };
        recs.push(Recommendation {
            kind: RecKind::CreateIndex,
            priority: priority.into(),
            title: format!("Create covering index on {}.{}", m.schema_name, m.table_name),
            object: format!("{}.{}", m.schema_name, m.table_name),
            rationale: format!(
                "SQL Server's missing-index DMV: {} seeks would benefit, avg query cost {:.2}, estimated improvement {:.0}%. Improvement measure {:.1}. Order key columns by selectivity and consolidate with existing indexes before applying.",
                m.user_seeks, m.avg_total_user_cost, m.avg_user_impact, score
            ),
            ddl,
            impact_score: score,
            metrics: vec![
                ("Seeks that benefit".into(), commas(m.user_seeks)),
                ("Est. cost reduction".into(), format!("~{:.0}%", m.avg_user_impact)),
                ("Avg query cost".into(), format!("{:.2}", m.avg_total_user_cost)),
            ],
            // These are SQL Server's own projections, not measured outcomes.
            confidence: "estimated".into(),
        });
    }

    // ---- DropIndex: write-only indexes (reads=0, many updates) -------------
    use std::collections::BTreeMap;
    let mut meta: BTreeMap<(String, String, String), &IndexMeta> = BTreeMap::new();
    for ix in &bundle.indexes {
        meta.insert(
            (ix.schema_name.to_lowercase(), ix.table_name.to_lowercase(), ix.index_name.to_lowercase()),
            ix,
        );
    }
    // Reserved KB per (schema, table, index) so a drop/merge rec can report the
    // storage it reclaims. Sum across partitions of the same index.
    let mut reserved_by_index: BTreeMap<(String, String, String), u64> = BTreeMap::new();
    for p in &bundle.partition_stats {
        if let Some(ix_name) = &p.index_name {
            *reserved_by_index
                .entry((p.schema_name.to_lowercase(), p.table_name.to_lowercase(), ix_name.to_lowercase()))
                .or_insert(0) += p.reserved_kb;
        }
    }
    for u in &bundle.index_usage {
        let reads = u.user_seeks + u.user_scans + u.user_lookups;
        if reads != 0 || u.user_updates <= 100 { continue; }
        let key = (u.schema_name.to_lowercase(), u.table_name.to_lowercase(), u.index_name.to_lowercase());
        // Never drop a PK or unique constraint index, or an obvious PK by name.
        if let Some(ix) = meta.get(&key) {
            if ix.is_primary_key || ix.is_unique { continue; }
        }
        if u.index_name.to_lowercase().starts_with("pk_") || u.index_name.eq_ignore_ascii_case("PK") { continue; }
        let priority = if u.user_updates >= 100_000 { "high" } else if u.user_updates >= 10_000 { "medium" } else { "low" };
        let reserved_kb = reserved_by_index.get(&key).copied().unwrap_or(0);
        recs.push(Recommendation {
            kind: RecKind::DropIndex,
            priority: priority.into(),
            title: format!("Drop unused index {} on {}.{}", u.index_name, u.schema_name, u.table_name),
            object: format!("{}.{}.{}", u.schema_name, u.table_name, u.index_name),
            rationale: format!(
                "{} updates and 0 reads since the last stats reset — the index is pure write tax (every INSERT/UPDATE/DELETE maintains it for no read benefit). Confirm across a representative window first (DMV counters reset on restart).",
                u.user_updates
            ),
            ddl: format!("DROP INDEX {} ON {}.{};", br(&u.index_name), br(&u.schema_name), br(&u.table_name)),
            impact_score: u.user_updates as f64,
            metrics: vec![
                ("Writes maintained".into(), commas(u.user_updates)),
                ("Reads".into(), "0".into()),
                ("Storage reclaimed".into(), format!("~{} MB", kb_to_mb(reserved_kb))),
            ],
            // Counters are measured directly from sys.dm_db_index_usage_stats.
            confidence: "observed".into(),
        });
    }

    // ---- MergeIndex: exact-duplicate key columns on the same table ---------
    let mut by_table: BTreeMap<(String, String), Vec<&IndexMeta>> = BTreeMap::new();
    for ix in &bundle.indexes {
        by_table.entry((ix.schema_name.clone(), ix.table_name.clone())).or_default().push(ix);
    }
    for ((schema, table), list) in &by_table {
        for i in 0..list.len() {
            for j in (i + 1)..list.len() {
                let (a, b) = (list[i], list[j]);
                let same = !a.key_columns.is_empty()
                    && a.key_columns.len() == b.key_columns.len()
                    && a.key_columns.iter().zip(&b.key_columns).all(|(x, y)| x.eq_ignore_ascii_case(y));
                if !same { continue; }
                // Keep the unique/PK one; drop the other.
                let (keep, drop) = if a.is_primary_key || a.is_unique { (a, b) }
                    else if b.is_primary_key || b.is_unique { (b, a) }
                    else { (a, b) };
                if drop.is_primary_key || drop.is_unique { continue; }
                let drop_reserved_kb = reserved_by_index
                    .get(&(schema.to_lowercase(), table.to_lowercase(), drop.index_name.to_lowercase()))
                    .copied()
                    .unwrap_or(0);
                recs.push(Recommendation {
                    kind: RecKind::MergeIndex,
                    priority: "medium".into(),
                    title: format!("Merge duplicate indexes on {}.{}", schema, table),
                    object: format!("{}.{}.{}", schema, table, drop.index_name),
                    rationale: format!(
                        "Indexes {} and {} have identical key columns ({}). One is redundant — every write maintains both. Fold any unique INCLUDE columns into {} and drop {}.",
                        keep.index_name, drop.index_name, drop.key_columns.join(", "), keep.index_name, drop.index_name
                    ),
                    ddl: format!(
                        "-- Merge needed INCLUDE columns into {} first, then:\nDROP INDEX {} ON {}.{};",
                        br(&keep.index_name), br(&drop.index_name), br(schema), br(table)
                    ),
                    impact_score: 5_000.0,
                    metrics: vec![
                        ("Storage".into(), format!("~{} MB", kb_to_mb(drop_reserved_kb))),
                        ("Reads".into(), "0 unique".into()),
                    ],
                    // Identical key columns are read directly from catalog metadata.
                    confidence: "observed".into(),
                });
            }
        }
    }

    // ---- ColumnstoreCandidate: big, scan-heavy, low-churn rowstore ---------
    // Aggregate row_count (max across partitions) + reserved_kb per table.
    let mut size_by_table: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
    for p in &bundle.partition_stats {
        let e = size_by_table.entry((p.schema_name.clone(), p.table_name.clone())).or_insert((0, 0));
        e.0 = e.0.max(p.row_count);
        e.1 += p.reserved_kb;
    }
    let mut usage_by_table: BTreeMap<(String, String), (u64, u64, u64)> = BTreeMap::new(); // (seeks, scans, updates)
    for u in &bundle.index_usage {
        let e = usage_by_table.entry((u.schema_name.clone(), u.table_name.clone())).or_insert((0, 0, 0));
        e.0 += u.user_seeks;
        e.1 += u.user_scans;
        e.2 += u.user_updates;
    }
    for ((schema, table), (rows, reserved_kb)) in &size_by_table {
        let (seeks, scans, updates) = usage_by_table.get(&(schema.clone(), table.clone())).copied().unwrap_or((0, 0, 0));
        let reads = seeks + scans;
        // Large, scan-dominated, low write churn → analytic table that wants a CCI.
        let big = *rows >= 1_000_000 || *reserved_kb >= 1_048_576; // ≥1M rows or ≥1GB
        let scan_heavy = scans > seeks && scans > 0;
        let low_churn = reads == 0 || (updates as f64) < (reads as f64) * 0.2;
        if big && scan_heavy && low_churn {
            let priority = if *reserved_kb >= 1_048_576 { "high" } else { "medium" };
            recs.push(Recommendation {
                kind: RecKind::ColumnstoreCandidate,
                priority: priority.into(),
                title: format!("Columnstore candidate: {}.{}", schema, table),
                object: format!("{}.{}", schema, table),
                rationale: format!(
                    "{} rows, {:.0} MB, scan-dominated ({} scans vs {} seeks) with low write churn ({} updates). Analytic/scan workloads on tables this size typically get 5–10× compression and large scan speedups from a clustered columnstore index.",
                    rows, (*reserved_kb as f64) / 1024.0, scans, seeks, updates
                ),
                ddl: format!(
                    "-- Validate workload is analytic/scan-heavy (not OLTP point lookups) first.\n-- Converting replaces the clustered rowstore; test in a non-prod copy.\nCREATE CLUSTERED COLUMNSTORE INDEX [CCI_{}] ON {}.{}\n  WITH (DROP_EXISTING = OFF, MAXDOP = 1);",
                    idfrag(table), br(schema), br(table)
                ),
                impact_score: *reserved_kb as f64,
                metrics: vec![
                    ("Rows".into(), commas(*rows)),
                    ("Size".into(), format!("~{} GB", kb_to_gb(*reserved_kb))),
                    ("Scans".into(), commas(scans)),
                ],
                // 5–10× compression is a rule-of-thumb, not a measured outcome.
                confidence: "heuristic".into(),
            });
        }
    }

    // Rank: priority bucket first, then impact within the bucket.
    recs.sort_by(|a, b| {
        priority_rank(&a.priority).cmp(&priority_rank(&b.priority))
            .then(b.impact_score.partial_cmp(&a.impact_score).unwrap_or(std::cmp::Ordering::Equal))
    });
    recs
}
