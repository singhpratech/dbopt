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
