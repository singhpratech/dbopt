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
    /// Per-(schema, table) workload frequency from Query Store / exec stats,
    /// used to ground missing-index recs in how often the benefiting query runs.
    /// ADDITIVE + OPTIONAL: defaults to `[]`, and the advisor degrades to the
    /// DMV's own seek counts when this is absent.
    #[serde(default)]
    pub workload: Vec<crate::advisor_workload::QueryWorkloadStat>,
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

    // --- Structural checks (schema-only; need NO workload and NO procs) ------
    // These fire from index metadata + row counts alone, so they surface real
    // pain on an idle, data-only database where the usage DMVs and the proc-body
    // scan find nothing. Thresholds are deliberately conservative to avoid the
    // false-positive noise that erodes DBA trust.
    {
        use std::collections::BTreeSet;
        let mut rows_by_table: BTreeMap<(String, String), u64> = BTreeMap::new();
        let mut heap_rows: BTreeMap<(String, String), u64> = BTreeMap::new();
        for p in &bundle.partition_stats {
            let key = (p.schema_name.clone(), p.table_name.clone());
            let e = rows_by_table.entry(key.clone()).or_insert(0);
            *e = (*e).max(p.row_count);
            if p.index_name.is_none() {
                let h = heap_rows.entry(key).or_insert(0);
                *h = (*h).max(p.row_count);
            }
        }
        let mut has_pk: BTreeSet<(String, String)> = BTreeSet::new();
        for ix in &bundle.indexes {
            if ix.is_primary_key {
                has_pk.insert((ix.schema_name.clone(), ix.table_name.clone()));
            }
        }
        // Heaps holding real data. Severity scales with size so a 262M-row heap
        // ranks far above a 1k-row one and pulls the grade accordingly.
        for ((schema, table), rows) in &heap_rows {
            if *rows < 1000 { continue; }
            let sev = if *rows >= 1_000_000 { Severity::Error }
                      else if *rows >= 100_000 { Severity::Warning }
                      else { Severity::Info };
            findings.push(Finding {
                rule: RuleId("structure.heap_table".into()),
                severity: sev,
                message: format!("{schema}.{table} is a heap (no clustered index) holding {rows} rows."),
                location: None,
                recommendation: Some("Heaps fragment over time, can't be range-seeked, and accumulate forward pointers on UPDATE that slow every read. Add a clustered index — usually the primary key, or a narrow ever-increasing surrogate. Deliberate staging / bulk-load heaps are fine, but document them.".into()),
            });
        }
        // Substantial tables with no primary key.
        for ((schema, table), rows) in &rows_by_table {
            if *rows < 100 { continue; }
            if has_pk.contains(&(schema.clone(), table.clone())) { continue; }
            let sev = if *rows >= 1_000_000 { Severity::Error } else { Severity::Warning };
            findings.push(Finding {
                rule: RuleId("structure.no_primary_key".into()),
                severity: sev,
                message: format!("{schema}.{table} has no primary key ({rows} rows)."),
                location: None,
                recommendation: Some("A primary key gives every row a stable identity and is required for reliable updates, replication, and most tooling. Add one — `ALTER TABLE [schema].[table] ADD CONSTRAINT PK_table PRIMARY KEY (...);` — a narrow surrogate key is fine if no natural key fits.".into()),
            });
        }
        // Over-wide clustered / primary key inflates every nonclustered index.
        for ix in &bundle.indexes {
            if ix.is_primary_key && ix.key_columns.len() >= 5 {
                findings.push(Finding {
                    rule: RuleId("structure.wide_clustered_key".into()),
                    severity: Severity::Info,
                    message: format!(
                        "{}.{} primary key spans {} columns ({}). Every nonclustered index stores the full key as its row locator, so a wide key inflates all of them.",
                        ix.schema_name, ix.table_name, ix.key_columns.len(), ix.key_columns.join(", ")
                    ),
                    location: None,
                    recommendation: Some("Keep the clustered key narrow, static, and ever-increasing — often a surrogate INT/BIGINT IDENTITY. Enforce the wide natural key with a separate UNIQUE constraint if you still need it.".into()),
                });
            }
        }
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
    // Cross-group consolidation first: the DMV emits one suggestion per query
    // shape, so the SAME table often gets several near-identical suggestions
    // that differ only by a trailing key column or an INCLUDE. Left un-merged
    // they'd become N overlapping indexes (write amplification + the very
    // duplication this advisor is meant to prevent). `consolidate_missing_indexes`
    // folds prefix-related suggestions into one superset index (widest key,
    // unioned INCLUDEs) before we emit any DDL. See its docs for the merge rule.
    let consolidated = consolidate_missing_indexes(&bundle.missing_indexes);
    for m in &consolidated {
        let keys: Vec<String> = m.equality_columns.iter().chain(m.inequality_columns.iter()).cloned().collect();
        if keys.is_empty() { continue; }
        // SQL Server's standard "improvement measure" — the raw benefit signal.
        let base_score = m.avg_total_user_cost * (m.avg_user_impact / 100.0) * (m.user_seeks.max(1) as f64);
        // Write-cost + workload grounding: down-rank sprawling indexes by their
        // maintenance cost and boost candidates whose table sees a hot query.
        // This is the lever that makes the advisor smarter than a raw DMV dump:
        // benefit ÷ write-cost, then floated by observed query frequency. When no
        // workload data is present it degrades to write-cost-only ranking.
        let ranking = crate::advisor_workload::rank_candidate(m, base_score, &bundle.workload);
        let score = ranking.adjusted_score;
        let write_cost = ranking.write_cost;
        let workload_phrase = crate::advisor_workload::workload_phrase(ranking.executions_per_day);
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
        // Compose rationale: DMV evidence + write-cost note + (optional) workload.
        let write_cost_note = match write_cost {
            crate::advisor_workload::WriteCost::High => " This index is WIDE — write cost: high, so every INSERT/UPDATE/DELETE pays to maintain it; we down-ranked it accordingly and you should trim key/INCLUDE columns to what queries actually need before applying.",
            crate::advisor_workload::WriteCost::Medium => " Write cost: medium — keep an eye on its INCLUDE list so it doesn't sprawl.",
            crate::advisor_workload::WriteCost::Low => " Write cost: low — narrow enough that maintenance overhead is modest.",
        };
        let workload_note = match &workload_phrase {
            Some(p) => format!(" It {p}, so it ranks ahead of indexes for colder queries."),
            None => String::new(),
        };
        let rationale = format!(
            "SQL Server's missing-index DMV: {} seeks would benefit, avg query cost {:.2}, estimated improvement {:.0}%.{}{} Order key columns by selectivity and consolidate with existing indexes before applying.",
            m.user_seeks, m.avg_total_user_cost, m.avg_user_impact, write_cost_note, workload_note
        );
        let mut metrics = vec![
            ("Seeks that benefit".into(), commas(m.user_seeks)),
            ("Est. cost reduction".into(), format!("~{:.0}%", m.avg_user_impact)),
            ("Avg query cost".into(), format!("{:.2}", m.avg_total_user_cost)),
            ("Write cost".into(), write_cost.label().to_string()),
        ];
        if let Some(per_day) = ranking.executions_per_day {
            if per_day > 0.0 {
                metrics.push(("Query runs".into(), format!("~{}×/day", if per_day >= 1.0 { format!("{:.0}", per_day.round()) } else { format!("{per_day:.1}") })));
            }
        }
        recs.push(Recommendation {
            kind: RecKind::CreateIndex,
            priority: priority.into(),
            title: format!("Create covering index on {}.{}", m.schema_name, m.table_name),
            object: format!("{}.{}", m.schema_name, m.table_name),
            rationale,
            ddl,
            impact_score: score,
            metrics,
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

    // ---- MergeIndex: left-PREFIX-redundant existing indexes ----------------
    // The exact-duplicate pass above only catches indexes whose key lists are
    // identical. The thinner case the old code missed: index A's key list is a
    // strict left prefix of index B's key list on the same table. Any query A
    // can satisfy, B can satisfy too (a composite index serves all of its leading
    // prefixes), so A is redundant — drop it and let B carry the load. This is
    // exactly what the community index-health script flags as a "borderline duplicate". We never
    // touch a PK/unique index (it enforces a constraint), and `redundant_existing_indexes`
    // dedupes so a key list that's a prefix of several survivors is only flagged once.
    for r in redundant_existing_indexes(&bundle.indexes) {
        let drop_reserved_kb = reserved_by_index
            .get(&(r.schema.to_lowercase(), r.table.to_lowercase(), r.redundant_index.to_lowercase()))
            .copied()
            .unwrap_or(0);
        recs.push(Recommendation {
            kind: RecKind::MergeIndex,
            priority: "medium".into(),
            title: format!("Drop prefix-redundant index {} on {}.{}", r.redundant_index, r.schema, r.table),
            object: format!("{}.{}.{}", r.schema, r.table, r.redundant_index),
            rationale: format!(
                "Index {} on key ({}) is a left prefix of {} on ({}). A composite index already serves every query that uses only its leading columns, so {} is redundant — it just doubles the write maintenance on this key. Confirm {} has any INCLUDE columns {} needs, then drop {}.",
                r.redundant_index, r.redundant_key.join(", "),
                r.superset_index, r.superset_key.join(", "),
                r.redundant_index, r.superset_index, r.redundant_index, r.redundant_index
            ),
            ddl: format!(
                "-- {} ({}) is a left prefix of {} ({}); fold any still-needed INCLUDE columns into {} first, then:\nDROP INDEX {} ON {}.{};",
                r.redundant_index, r.redundant_key.join(", "),
                r.superset_index, r.superset_key.join(", "),
                br(&r.superset_index),
                br(&r.redundant_index), br(&r.schema), br(&r.table)
            ),
            impact_score: 4_000.0,
            metrics: vec![
                ("Storage".into(), format!("~{} MB", kb_to_mb(drop_reserved_kb))),
                ("Reads".into(), "0 unique".into()),
            ],
            // Prefix relationship is read directly from catalog key-column metadata.
            confidence: "observed".into(),
        });
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

// ===========================================================================
// Index-advisor cross-group de-dup — closes the "thinner the community index-health script" gap.
//
// The base `advise()` only merged EXACT same-table duplicate EXISTING indexes
// and emitted one CreateIndex per raw missing-index DMV row. That leaves two
// real gaps that a serious index advisor must close:
//   (a) the missing-index DMV fans out one suggestion per query shape, so the
//       SAME table gets several prefix-overlapping suggestions. Applied naively
//       they become N overlapping indexes — the exact write-amplification the
//       advisor exists to prevent. We consolidate them into one superset index.
//   (b) an EXISTING index whose key list is a strict left prefix of another
//       index's key list on the same table is redundant (a composite index
//       serves all of its leading prefixes) and should be dropped.
//   (c) an EXISTING index with writes but ~zero reads is pure write tax and is
//       a drop candidate — already handled by the DropIndex pass in `advise()`;
//       `unused_existing_indexes` factors that logic out for reuse + testing.
//
// All three are pure functions over the bundle so they unit-test in isolation,
// and the conservative merge rules below keep false positives near zero.
// ===========================================================================

/// Case-insensitive equality on an identifier fragment, ignoring brackets and
/// surrounding whitespace (so `[CustomerId]` == `customerid`).
fn col_eq(a: &str, b: &str) -> bool {
    unbr(a).eq_ignore_ascii_case(&unbr(b))
}

/// True iff `prefix`'s columns are a prefix (or equal) of `full`'s columns,
/// compared case-insensitively and bracket-insensitively, IN ORDER. An empty
/// `prefix` is never treated as a prefix (it carries no key, nothing to merge).
fn is_key_prefix(prefix: &[String], full: &[String]) -> bool {
    if prefix.is_empty() || prefix.len() > full.len() { return false; }
    prefix.iter().zip(full.iter()).all(|(p, f)| col_eq(p, f))
}

/// Union INCLUDE columns preserving first-seen order, case/bracket-insensitive.
fn union_includes(into: &mut Vec<String>, extra: &[String]) {
    for c in extra {
        if !into.iter().any(|x| col_eq(x, c)) && !c.trim().is_empty() {
            into.push(c.clone());
        }
    }
}

/// (a) CROSS-GROUP missing-index consolidation.
///
/// The DMV's missing-index suggestions are grouped per query shape, so one
/// table frequently has several suggestions that share leading equality
/// columns or where one key is a left prefix of another. Creating all of them
/// produces overlapping, write-amplifying indexes. This folds prefix-related
/// suggestions on the SAME (schema, table) into one superset suggestion:
///   * the WIDEST key wins (the prefix one is absorbed),
///   * INCLUDE columns are UNIONED,
///   * impact metrics are aggregated (max user-impact + cost, summed seeks),
///     so the consolidated rec still ranks on the strongest evidence.
///
/// Conservative by design — we ONLY merge when one suggestion's full key
/// (equality columns followed by inequality columns, the order the DMV uses to
/// build the key) is a clean ordered prefix of another's. Suggestions that
/// merely overlap without a prefix relationship are left separate, because
/// reordering key columns can change selectivity and we will not guess.
pub fn consolidate_missing_indexes(missing: &[MissingIndex]) -> Vec<MissingIndex> {
    // The DMV builds the index key as equality columns first, then inequality
    // columns. Compare on that full ordered key.
    fn full_key(m: &MissingIndex) -> Vec<String> {
        m.equality_columns.iter().chain(m.inequality_columns.iter()).cloned().collect()
    }

    // Group by (schema, table), case-insensitively, preserving input order.
    let mut groups: BTreeMap<(String, String), Vec<MissingIndex>> = BTreeMap::new();
    for m in missing {
        if full_key(m).is_empty() { continue; }
        groups
            .entry((m.schema_name.to_lowercase(), m.table_name.to_lowercase()))
            .or_default()
            .push(m.clone());
    }

    let mut out: Vec<MissingIndex> = Vec::new();
    for (_, mut list) in groups {
        // Greedy fixpoint merge: repeatedly fold any pair in a prefix relation
        // into the wider one until no more merges are possible. O(n^2) per
        // group, and groups are tiny (a handful of suggestions), so this is fine.
        let mut changed = true;
        while changed {
            changed = false;
            'outer: for i in 0..list.len() {
                for j in 0..list.len() {
                    if i == j { continue; }
                    let ki = full_key(&list[i]);
                    let kj = full_key(&list[j]);
                    // Merge j INTO i when j's key is a (possibly equal) prefix of
                    // i's key — i is the superset and survives. When the keys are
                    // identical, `i < j` ensures we pick a single survivor and
                    // don't ping-pong forever.
                    let j_is_prefix_of_i = is_key_prefix(&kj, &ki);
                    let identical = ki.len() == kj.len() && j_is_prefix_of_i;
                    if j_is_prefix_of_i && (!identical || i < j) {
                        // Absorb j into i: widest key already on i; union INCLUDEs
                        // (also pull j's KEY columns beyond i's into i's INCLUDE?
                        // No — j is a prefix, so it has no columns i lacks).
                        let absorbed = list.remove(j);
                        // Index i may have shifted if j < i.
                        let i2 = if j < i { i - 1 } else { i };
                        union_includes(&mut list[i2].included_columns, &absorbed.included_columns);
                        // Aggregate evidence onto the survivor.
                        list[i2].avg_user_impact = list[i2].avg_user_impact.max(absorbed.avg_user_impact);
                        list[i2].avg_total_user_cost = list[i2].avg_total_user_cost.max(absorbed.avg_total_user_cost);
                        list[i2].user_seeks = list[i2].user_seeks.saturating_add(absorbed.user_seeks);
                        changed = true;
                        break 'outer;
                    }
                }
            }
        }
        out.extend(list);
    }
    out
}

/// One left-prefix-redundant EXISTING index: `redundant_index` can be dropped
/// because its key is a strict left prefix of `superset_index`'s key on the
/// same table.
#[derive(Debug, Clone, PartialEq)]
pub struct RedundantIndexFinding {
    pub schema: String,
    pub table: String,
    pub redundant_index: String,
    pub redundant_key: Vec<String>,
    pub superset_index: String,
    pub superset_key: Vec<String>,
}

/// (b) Detect EXISTING indexes whose key list is a strict left prefix of
/// another index's key list on the same table — the prefix one is redundant.
///
/// Conservative guards (false positives are the worst outcome here, because the
/// "fix" is a DROP):
///   * STRICT prefix only (shorter than the superset). Exact duplicates are
///     already handled by `advise()`'s exact-dup MergeIndex pass; emitting them
///     here too would double-report.
///   * never propose dropping a PRIMARY KEY or UNIQUE index — it enforces a
///     constraint and a query plan may depend on its uniqueness guarantee.
///     A unique index is also NOT redundant with a non-unique superset.
///   * a redundant index is reported at most once even if several wider indexes
///     contain it as a prefix (pick the first survivor deterministically).
pub fn redundant_existing_indexes(indexes: &[IndexMeta]) -> Vec<RedundantIndexFinding> {
    let mut by_table: BTreeMap<(String, String), Vec<&IndexMeta>> = BTreeMap::new();
    for ix in indexes {
        if ix.key_columns.is_empty() { continue; }
        by_table
            .entry((ix.schema_name.clone(), ix.table_name.clone()))
            .or_default()
            .push(ix);
    }

    let mut out: Vec<RedundantIndexFinding> = Vec::new();
    for ((schema, table), list) in &by_table {
        for i in 0..list.len() {
            let cand = list[i];
            // Never drop something that enforces a constraint.
            if cand.is_primary_key || cand.is_unique { continue; }
            // Find a STRICT superset: same leading key, strictly longer.
            let superset = list.iter().enumerate().find(|(j, other)| {
                *j != i
                    && other.key_columns.len() > cand.key_columns.len()
                    && is_key_prefix(&cand.key_columns, &other.key_columns)
            });
            if let Some((_, sup)) = superset {
                out.push(RedundantIndexFinding {
                    schema: schema.clone(),
                    table: table.clone(),
                    redundant_index: cand.index_name.clone(),
                    redundant_key: cand.key_columns.clone(),
                    superset_index: sup.index_name.clone(),
                    superset_key: sup.key_columns.clone(),
                });
            }
        }
    }
    out
}

/// One clearly-unused EXISTING index: writes accrued but reads are ~zero.
#[derive(Debug, Clone, PartialEq)]
pub struct UnusedIndexFinding {
    pub schema: String,
    pub table: String,
    pub index_name: String,
    pub user_updates: u64,
}

/// (c) Detect EXISTING indexes that take writes but serve ~zero reads (pure
/// write tax). Mirrors the DropIndex logic already wired into `advise()`,
/// factored out as a pure function for reuse and direct testing. Same
/// conservative guards: requires a meaningful write count and skips PK/unique
/// and PK-named indexes (`meta` lets the caller pass catalog metadata so we can
/// honour the is_primary_key / is_unique flags, not just the name heuristic).
pub fn unused_existing_indexes(
    usage: &[IndexUsage],
    indexes: &[IndexMeta],
    min_updates: u64,
) -> Vec<UnusedIndexFinding> {
    let mut meta: BTreeMap<(String, String, String), &IndexMeta> = BTreeMap::new();
    for ix in indexes {
        meta.insert(
            (ix.schema_name.to_lowercase(), ix.table_name.to_lowercase(), ix.index_name.to_lowercase()),
            ix,
        );
    }
    let mut out = Vec::new();
    for u in usage {
        let reads = u.user_seeks + u.user_scans + u.user_lookups;
        if reads != 0 || u.user_updates <= min_updates { continue; }
        let key = (u.schema_name.to_lowercase(), u.table_name.to_lowercase(), u.index_name.to_lowercase());
        if let Some(ix) = meta.get(&key) {
            if ix.is_primary_key || ix.is_unique { continue; }
        }
        if u.index_name.to_lowercase().starts_with("pk_") || u.index_name.eq_ignore_ascii_case("PK") { continue; }
        out.push(UnusedIndexFinding {
            schema: u.schema_name.clone(),
            table: u.table_name.clone(),
            index_name: u.index_name.clone(),
            user_updates: u.user_updates,
        });
    }
    out
}

#[cfg(test)]
mod dedup_tests {
    use super::*;

    fn mi(table: &str, eq: &[&str], ineq: &[&str], inc: &[&str], impact: f64, seeks: u64, cost: f64) -> MissingIndex {
        MissingIndex {
            schema_name: "dbo".into(),
            table_name: table.into(),
            equality_columns: eq.iter().map(|s| s.to_string()).collect(),
            inequality_columns: ineq.iter().map(|s| s.to_string()).collect(),
            included_columns: inc.iter().map(|s| s.to_string()).collect(),
            avg_user_impact: impact,
            user_seeks: seeks,
            avg_total_user_cost: cost,
        }
    }

    fn ix(table: &str, name: &str, keys: &[&str], unique: bool, pk: bool) -> IndexMeta {
        IndexMeta {
            schema_name: "dbo".into(),
            table_name: table.into(),
            index_name: name.into(),
            is_unique: unique,
            is_primary_key: pk,
            key_columns: keys.iter().map(|s| s.to_string()).collect(),
            included_columns: vec![],
        }
    }

    fn usage(table: &str, name: &str, seeks: u64, scans: u64, lookups: u64, updates: u64) -> IndexUsage {
        IndexUsage {
            database_name: "db".into(),
            schema_name: "dbo".into(),
            table_name: table.into(),
            index_name: name.into(),
            user_seeks: seeks,
            user_scans: scans,
            user_lookups: lookups,
            user_updates: updates,
        }
    }

    // ---- (a) consolidate_missing_indexes -----------------------------------

    #[test]
    fn consolidate_merges_prefix_suggestions_into_superset() {
        // Two suggestions on the same table: ([CustomerId]) is a left prefix of
        // ([CustomerId],[OrderDate]). They must collapse to ONE superset index
        // with the wider key and the UNION of INCLUDE columns.
        let missing = vec![
            mi("Orders", &["CustomerId"], &[], &["Total"], 60.0, 100, 5.0),
            mi("Orders", &["CustomerId"], &["OrderDate"], &["Amount"], 90.0, 300, 12.0),
        ];
        let out = consolidate_missing_indexes(&missing);
        assert_eq!(out.len(), 1, "prefix suggestions must consolidate to one: {out:#?}");
        let m = &out[0];
        // Widest key wins (equality CustomerId + inequality OrderDate).
        assert_eq!(m.equality_columns, vec!["CustomerId"]);
        assert_eq!(m.inequality_columns, vec!["OrderDate"]);
        // INCLUDEs unioned.
        assert!(m.included_columns.iter().any(|c| c.eq_ignore_ascii_case("Total")));
        assert!(m.included_columns.iter().any(|c| c.eq_ignore_ascii_case("Amount")));
        // Evidence aggregated: max impact/cost, summed seeks.
        assert_eq!(m.user_seeks, 400);
        assert!((m.avg_user_impact - 90.0).abs() < 1e-9);
        assert!((m.avg_total_user_cost - 12.0).abs() < 1e-9);
    }

    #[test]
    fn consolidate_keeps_unrelated_keys_separate() {
        // NEGATIVE: same table but the keys are NOT in a prefix relationship
        // (different leading column). Reordering could change selectivity, so we
        // must NOT merge them.
        let missing = vec![
            mi("Orders", &["CustomerId"], &[], &[], 50.0, 10, 1.0),
            mi("Orders", &["ProductId"], &[], &[], 50.0, 10, 1.0),
        ];
        let out = consolidate_missing_indexes(&missing);
        assert_eq!(out.len(), 2, "non-prefix suggestions must stay separate: {out:#?}");
    }

    #[test]
    fn consolidate_does_not_cross_tables() {
        // NEGATIVE: identical-shaped suggestions on DIFFERENT tables are not
        // duplicates and must both survive.
        let missing = vec![
            mi("Orders", &["CustomerId"], &[], &[], 50.0, 10, 1.0),
            mi("Invoices", &["CustomerId"], &[], &[], 50.0, 10, 1.0),
        ];
        let out = consolidate_missing_indexes(&missing);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn consolidate_advise_emits_single_create_for_prefix_group() {
        // End-to-end through advise(): the prefix group yields exactly one
        // CreateIndex carrying the superset key + unioned INCLUDEs.
        let bundle = DmvBundle {
            missing_indexes: vec![
                mi("Orders", &["CustomerId"], &[], &["Total"], 60.0, 100, 5.0),
                mi("Orders", &["CustomerId"], &["OrderDate"], &["Amount"], 90.0, 300, 12.0),
            ],
            ..Default::default()
        };
        let recs = advise(&bundle);
        let creates: Vec<_> = recs.iter().filter(|r| r.kind == RecKind::CreateIndex).collect();
        assert_eq!(creates.len(), 1, "one consolidated CreateIndex expected: {creates:#?}");
        let ddl = &creates[0].ddl;
        assert!(ddl.contains("[CustomerId]") && ddl.contains("[OrderDate]"), "key: {ddl}");
        assert!(ddl.contains("[Total]") && ddl.contains("[Amount]"), "include: {ddl}");
    }

    // ---- (b) redundant_existing_indexes ------------------------------------

    #[test]
    fn redundant_flags_left_prefix_existing_index() {
        // IX_a (CustomerId) is a strict prefix of IX_b (CustomerId, OrderDate).
        let idx = vec![
            ix("Orders", "IX_a", &["CustomerId"], false, false),
            ix("Orders", "IX_b", &["CustomerId", "OrderDate"], false, false),
        ];
        let out = redundant_existing_indexes(&idx);
        assert_eq!(out.len(), 1, "{out:#?}");
        assert_eq!(out[0].redundant_index, "IX_a");
        assert_eq!(out[0].superset_index, "IX_b");
    }

    #[test]
    fn redundant_advise_emits_merge_drop_for_prefix() {
        let bundle = DmvBundle {
            indexes: vec![
                ix("Orders", "IX_a", &["CustomerId"], false, false),
                ix("Orders", "IX_b", &["CustomerId", "OrderDate"], false, false),
            ],
            ..Default::default()
        };
        let recs = advise(&bundle);
        let merge = recs.iter().find(|r| r.kind == RecKind::MergeIndex && r.object.ends_with("IX_a"));
        let merge = merge.expect("expected a MergeIndex drop rec for the prefix index");
        assert!(merge.ddl.contains("DROP INDEX [IX_a]"), "ddl: {}", merge.ddl);
        assert!(merge.ddl.contains("[IX_b]"), "ddl should name the superset: {}", merge.ddl);
    }

    #[test]
    fn redundant_never_drops_pk_or_unique_prefix() {
        // NEGATIVE: a UNIQUE / PK index that is a prefix of another index is NOT
        // redundant — it enforces a constraint. Must not be flagged.
        let idx = vec![
            ix("Orders", "PK_Orders", &["Id"], true, true),
            ix("Orders", "IX_super", &["Id", "OrderDate"], false, false),
            ix("Orders", "UQ_email", &["Email"], true, false),
            ix("Orders", "IX_emailx", &["Email", "Tenant"], false, false),
        ];
        let out = redundant_existing_indexes(&idx);
        assert!(out.is_empty(), "unique/PK prefixes must not be flagged: {out:#?}");
    }

    #[test]
    fn redundant_ignores_exact_duplicates() {
        // NEGATIVE: exact duplicates (same length) are handled by advise()'s own
        // exact-dup pass, not here. STRICT prefix only.
        let idx = vec![
            ix("Orders", "IX_a", &["CustomerId"], false, false),
            ix("Orders", "IX_b", &["CustomerId"], false, false),
        ];
        let out = redundant_existing_indexes(&idx);
        assert!(out.is_empty(), "exact dups are out of scope here: {out:#?}");
    }

    #[test]
    fn redundant_ignores_different_leading_column() {
        // NEGATIVE: shares a column but not as a leading prefix.
        let idx = vec![
            ix("Orders", "IX_a", &["OrderDate"], false, false),
            ix("Orders", "IX_b", &["CustomerId", "OrderDate"], false, false),
        ];
        let out = redundant_existing_indexes(&idx);
        assert!(out.is_empty(), "{out:#?}");
    }

    // ---- (c) unused_existing_indexes ---------------------------------------

    #[test]
    fn unused_flags_write_only_index() {
        let usage = vec![usage("Orders", "IX_writeonly", 0, 0, 0, 50_000)];
        let idx = vec![ix("Orders", "IX_writeonly", &["Status"], false, false)];
        let out = unused_existing_indexes(&usage, &idx, 100);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].index_name, "IX_writeonly");
        assert_eq!(out[0].user_updates, 50_000);
    }

    #[test]
    fn unused_skips_indexes_with_reads_pk_and_low_writes() {
        // NEGATIVE: an index that is read, a PK index, and a low-write index must
        // all be left alone.
        let usage = vec![
            usage("Orders", "IX_read", 0, 5, 0, 50_000),     // has reads
            usage("Orders", "PK_Orders", 0, 0, 0, 50_000),   // pk by metadata
            usage("Orders", "IX_tiny", 0, 0, 0, 10),         // below threshold
        ];
        let idx = vec![
            ix("Orders", "IX_read", &["A"], false, false),
            ix("Orders", "PK_Orders", &["Id"], true, true),
            ix("Orders", "IX_tiny", &["B"], false, false),
        ];
        let out = unused_existing_indexes(&usage, &idx, 100);
        assert!(out.is_empty(), "none should be flagged: {out:#?}");
    }

    // ---- write-cost + workload grounding through advise() ------------------

    #[test]
    fn advise_surfaces_write_cost_label_on_create_index() {
        // A narrow candidate must carry a "Write cost: low" metric chip and the
        // rationale must mention write cost, so a DBA can trust the ranking.
        let bundle = DmvBundle {
            missing_indexes: vec![mi("Orders", &["CustomerId"], &[], &[], 90.0, 1000, 10.0)],
            ..Default::default()
        };
        let recs = advise(&bundle);
        let ci = recs.iter().find(|r| r.kind == RecKind::CreateIndex).expect("create rec");
        assert!(
            ci.metrics.iter().any(|(k, v)| k == "Write cost" && v == "low"),
            "expected a low write-cost chip: {:#?}",
            ci.metrics
        );
        assert!(ci.rationale.to_lowercase().contains("write cost"), "rationale: {}", ci.rationale);
    }

    #[test]
    fn advise_downranks_wide_index_below_narrow_at_equal_dmv_benefit() {
        // Two candidates on DIFFERENT tables with the SAME raw DMV numbers; the
        // sprawling one must rank BELOW the narrow one purely on write cost.
        // (Different tables so consolidation leaves them separate.)
        let bundle = DmvBundle {
            missing_indexes: vec![
                mi("Narrow", &["A"], &[], &[], 90.0, 1000, 10.0),
                mi("Wide", &["A", "B", "C", "D", "E", "F"], &[], &["X", "Y", "Z", "W"], 90.0, 1000, 10.0),
            ],
            ..Default::default()
        };
        let recs = advise(&bundle);
        let narrow = recs.iter().find(|r| r.object.ends_with("Narrow")).unwrap();
        let wide = recs.iter().find(|r| r.object.ends_with("Wide")).unwrap();
        assert!(narrow.impact_score > wide.impact_score, "narrow {} should beat wide {}", narrow.impact_score, wide.impact_score);
        assert!(wide.metrics.iter().any(|(k, v)| k == "Write cost" && v == "high"), "wide should be high write cost: {:#?}", wide.metrics);
    }

    #[test]
    fn advise_floats_hot_query_and_reads_runs_per_day() {
        // Identical candidate shape + DMV numbers on two tables, but one table
        // sees a hot query in the workload. The hot one must rank higher and its
        // rationale must read "helps a query that runs ~N×/day".
        let bundle = DmvBundle {
            missing_indexes: vec![
                mi("Hot", &["A"], &[], &[], 90.0, 1000, 10.0),
                mi("Cold", &["A"], &[], &[], 90.0, 1000, 10.0),
            ],
            workload: vec![
                crate::advisor_workload::QueryWorkloadStat { schema_name: "dbo".into(), table_name: "Hot".into(), execution_count: 50_000, window_hours: 24.0 },
                crate::advisor_workload::QueryWorkloadStat { schema_name: "dbo".into(), table_name: "Cold".into(), execution_count: 3, window_hours: 24.0 },
            ],
            ..Default::default()
        };
        let recs = advise(&bundle);
        let hot = recs.iter().find(|r| r.object.ends_with("Hot")).unwrap();
        let cold = recs.iter().find(|r| r.object.ends_with("Cold")).unwrap();
        assert!(hot.impact_score > cold.impact_score, "hot {} should beat cold {}", hot.impact_score, cold.impact_score);
        assert!(hot.rationale.contains("runs ~"), "hot rationale should cite a per-day rate: {}", hot.rationale);
        assert!(hot.metrics.iter().any(|(k, _)| k == "Query runs"), "hot should carry a Query runs chip: {:#?}", hot.metrics);
    }

    #[test]
    fn advise_without_workload_still_emits_create_and_omits_runs_chip() {
        // Graceful degrade: no workload → still a valid CreateIndex, no "Query
        // runs" chip, no misleading "0×/day".
        let bundle = DmvBundle {
            missing_indexes: vec![mi("Orders", &["CustomerId"], &[], &[], 90.0, 1000, 10.0)],
            ..Default::default()
        };
        let recs = advise(&bundle);
        let ci = recs.iter().find(|r| r.kind == RecKind::CreateIndex).unwrap();
        assert!(!ci.metrics.iter().any(|(k, _)| k == "Query runs"), "no runs chip without workload: {:#?}", ci.metrics);
        assert!(!ci.rationale.contains("runs ~"), "no per-day phrase without workload: {}", ci.rationale);
    }
}
