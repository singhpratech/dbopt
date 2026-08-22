use crate::findings::{Finding, ObjectRef, RuleId, Severity};
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
    /// The database these DMV rows were actually read from (`DB_NAME()` at
    /// collection time). A server-level connection with no database selected
    /// lands in the login's default database — usually `master` — where
    /// `is_ms_shipped = 0` filters everything out and the scan looks "clean"
    /// when it simply looked in the wrong place. Recording the scope is what
    /// lets the caller say so out loud instead of reporting a silent blank.
    #[serde(default)]
    pub scanned_database: String,
    /// When the instance last started (`sys.dm_os_sys_info.sqlserver_start_time`,
    /// RFC 3339 UTC). Every `sys.dm_db_*_stats` counter in this bundle has been
    /// accumulating only since then. ADDITIVE + OPTIONAL: `None` when the
    /// collector could not read it (offline bundle, no VIEW SERVER STATE).
    #[serde(default)]
    pub counters_since: Option<String>,
    /// Seconds between `counters_since` and collection time — the lifetime of
    /// every usage counter here. The advisor stamps it on each usage-based
    /// recommendation and downgrades confidence while it is under a day.
    #[serde(default)]
    pub counter_age_secs: Option<i64>,
    /// Per-table persistence of missing-index suggestions from the monitor's
    /// daily DMV snapshots (see `sentinel::poll::missing_index`). Lets a
    /// create-index rec say "seen on N of the last M monitored days" instead of
    /// presenting one reading of a DMV that forgets on restart. ADDITIVE:
    /// defaults to `[]` (unmonitored server → no claim either way).
    #[serde(default)]
    pub missing_index_history: Vec<MissingIndexHistory>,
    /// Per-index read/write persistence from the monitor's usage-delta
    /// captures (see `sentinel::poll::index_usage`). Lets a "never used since
    /// restart" rec be promoted once the monitor has watched the index stay
    /// idle for days. ADDITIVE: defaults to `[]` (unmonitored → no claim).
    #[serde(default)]
    pub index_usage_history: Vec<IndexUsageHistory>,
    /// Physical shape of the larger indexes/heaps from
    /// `sys.dm_db_index_physical_stats` (LIMITED for indexes, SAMPLED for
    /// heaps). Collected only on the advise/health path and time-boxed; `[]`
    /// when skipped, which the advisor treats as "not measured", never as 0 %.
    #[serde(default)]
    pub physical: Vec<IndexPhysical>,
    /// Statistics freshness from `sys.dm_db_stats_properties` (rows ≥ 1,000).
    #[serde(default)]
    pub stats: Vec<StatsProperties>,
    /// Columns declared with a deprecated LOB type (`text` / `ntext` / `image`).
    #[serde(default)]
    pub deprecated_columns: Vec<DeprecatedColumn>,
    /// Query-Store queries whose per-execution cost swings by ≥ 10× across
    /// ≥ 2 cached plans — the parameter-sniffing signature. `[]` when Query
    /// Store is off or the catalog views are unreadable.
    #[serde(default)]
    pub query_skew: Vec<QuerySkew>,
}

/// Days the monitor watched an index and how many of those days it was read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexUsageHistory {
    pub schema_name: String,
    pub table_name: String,
    pub index_name: String,
    /// Distinct capture days the monitor has index-usage deltas for on this instance.
    pub days_observed: u32,
    /// Distinct capture days on which this index recorded any seek/scan/lookup.
    pub days_with_reads: u32,
    /// Distinct capture days on which this index recorded any write.
    pub days_with_writes: u32,
}

/// One leaf-level row of `sys.dm_db_index_physical_stats` for an index or heap.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexPhysical {
    pub schema_name: String,
    pub table_name: String,
    /// `None` for the heap (index_id 0).
    pub index_name: Option<String>,
    pub index_id: i32,
    pub page_count: u64,
    pub avg_fragmentation_pct: f64,
    /// Leaf page density; `None` in LIMITED mode (not measured).
    pub avg_page_space_used_pct: Option<f64>,
    /// Heap forward pointers; `None` when not measured (LIMITED mode / not a heap).
    pub forwarded_record_count: Option<u64>,
    /// `sys.indexes.fill_factor` (0 = server default = 100).
    pub fill_factor: u8,
}

/// One statistics object with its freshness counters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatsProperties {
    pub schema_name: String,
    pub table_name: String,
    pub stats_name: String,
    /// True when the stats object backs an index of the same name.
    pub is_index_stat: bool,
    pub no_recompute: bool,
    /// Rows the histogram was built from (`dm_db_stats_properties.rows`).
    pub rows: u64,
    pub rows_sampled: u64,
    pub modification_counter: u64,
    /// RFC 3339 of `last_updated`, when known.
    pub last_updated: Option<String>,
    /// Current table row count from partition stats, when the collector had it.
    pub table_rows: Option<u64>,
}

/// A column declared with a deprecated LOB type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeprecatedColumn {
    pub schema_name: String,
    pub table_name: String,
    pub column_name: String,
    /// `text` | `ntext` | `image`
    pub type_name: String,
    pub is_nullable: bool,
}

/// A Query-Store query whose logical-read cost swings widely across plans.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuerySkew {
    pub query_id: i64,
    /// `schema.proc` when the query belongs to a module, else `None` (ad hoc).
    pub object_name: Option<String>,
    pub plan_count: u32,
    pub executions: u64,
    pub avg_logical_reads: u64,
    pub max_logical_reads: u64,
    pub avg_duration_ms: f64,
    pub max_duration_ms: f64,
    pub sql_text: String,
}

/// How persistently the missing-index DMV has suggested an index on a table
/// across the monitor's daily captures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingIndexHistory {
    pub schema_name: String,
    pub table_name: String,
    /// Distinct capture days on which the DMV suggested an index on this table.
    pub days_seen: u32,
    /// Distinct capture days the monitor has for this instance in the window.
    pub days_observed: u32,
}

/// Usage counters younger than this are a sample, not a verdict: confidence
/// on usage-based recommendations is downgraded to `estimated` below it.
pub const COUNTER_AGE_YOUNG_SECS: i64 = 24 * 3600;

/// Human phrase for the counter lifetime, e.g. `counters cover 0.8 h since
/// restart` / `counters cover 2,064 h (86 d) since restart`.
pub fn counter_age_phrase(age_secs: i64) -> String {
    let h = age_secs.max(0) as f64 / 3600.0;
    if h < 10.0 {
        format!("counters cover {h:.1} h since restart")
    } else if age_secs < 48 * 3600 {
        format!("counters cover {} h since restart", h.round() as u64)
    } else {
        format!("counters cover {} h ({} d) since restart", commas(h.round() as u64), age_secs / 86_400)
    }
}

/// Stamp the counter lifetime on every usage-based recommendation: a rationale
/// suffix (with the restart instant when known), an evidence chip, and a
/// confidence downgrade from `observed` to `estimated` while the counters are
/// younger than [`COUNTER_AGE_YOUNG_SECS`]. "0 reads since restart" measured
/// over ten minutes is not an observation of the index being unused.
///
/// Every rec kind the advisor emits is usage-based (seeks, reads/writes,
/// scans, ×/day), so all of them carry the stamp. `heuristic` is never
/// upgraded or downgraded — it is already the weakest tier.
pub fn apply_counter_age(recs: &mut [Recommendation], counter_age_secs: Option<i64>, counters_since: Option<&str>) {
    let Some(age) = counter_age_secs else { return };
    let phrase = counter_age_phrase(age);
    let since = counters_since.map(|s| format!(" ({s})")).unwrap_or_default();
    let young = age < COUNTER_AGE_YOUNG_SECS;
    for r in recs.iter_mut() {
        if !r.kind.is_usage_based() { continue; }
        let mut suffix = format!(" Usage {phrase}{since}");
        if young {
            suffix.push_str("; under 24 h of counters is a sample, not a verdict — re-check after a representative window");
        }
        suffix.push('.');
        r.rationale.push_str(&suffix);
        r.metrics.push(("Counters since restart".into(), format!("{:.1} h", age.max(0) as f64 / 3600.0)));
        if young && r.confidence == "observed" {
            r.confidence = "estimated".into();
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexUsage {
    pub database_name: String,
    pub schema_name: String,
    pub table_name: String,
    pub index_name: String,
    pub user_seeks: u64,
    pub user_scans: u64,
    pub user_lookups: u64,
    pub user_updates: u64,
    /// True when `sys.dm_db_index_usage_stats` had NO row for this index (the
    /// collector LEFT JOINs and zero-fills so every index is listed). A
    /// zero-filled row is "not observed since restart", not "measured 0".
    #[serde(default)]
    pub no_stats_row: bool,
    /// `sys.indexes.index_id` (0 = heap, 1 = clustered). Optional on the wire;
    /// when absent the `(heap)` name convention identifies the heap row.
    #[serde(default)]
    pub index_id: Option<i32>,
}

impl IndexUsage {
    /// True for the heap row (index_id 0 / `(heap)` placeholder name).
    pub fn is_heap(&self) -> bool {
        self.index_id == Some(0) || self.index_name.eq_ignore_ascii_case("(heap)") || self.index_name.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexMeta {
    pub schema_name: String,
    pub table_name: String,
    pub index_name: String,
    pub is_unique: bool,
    pub is_primary_key: bool,
    /// True when this is the table's clustered index (`sys.indexes.index_id = 1`).
    /// Optional on the wire so older bundles / hand-written scenario files still
    /// parse; when absent the columnstore pass falls back to "the PK is clustered"
    /// (SQL Server's default) for tables that are not heaps.
    #[serde(default)]
    pub is_clustered: bool,
    pub key_columns: Vec<String>,
    pub included_columns: Vec<String>,
    /// Declared type of each key column, parallel to `key_columns` (e.g.
    /// `uniqueidentifier`). ADDITIVE: `[]` on older bundles.
    #[serde(default)]
    pub key_column_types: Vec<String>,
    /// Sum of the key columns' declared max byte length (`-1`/MAX counted as
    /// 8000). `None` when not collected.
    #[serde(default)]
    pub key_bytes: Option<u32>,
    /// True when a key column carries a `NEWID()` default (random GUID inserts).
    #[serde(default)]
    pub has_newid_default: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartitionStats {
    pub schema_name: String,
    pub table_name: String,
    pub index_name: Option<String>,
    pub row_count: u64,
    pub reserved_kb: u64,
    pub used_kb: u64,
    pub data_kb: u64,
    /// `sys.indexes.index_id`; only 0 (heap) and 1 (clustered) carry the
    /// table's row count — nonclustered rows are locators, not table rows.
    #[serde(default)]
    pub index_id: Option<i32>,
}

/// Table row counts from partition stats, keyed on lower-cased `schema.table`.
/// Counts ONLY the heap / clustered partitions (index_id 0 or 1) when the
/// bundle says which they are; older bundles without `index_id` fall back to
/// the max across the table's indexes (every index holds one row per table
/// row, so max is right and sum is wrong — summing once reported a 300,000-row
/// table as 600,000).
pub fn table_row_counts(partition_stats: &[PartitionStats]) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    let mut base: BTreeMap<String, u64> = BTreeMap::new();
    for p in partition_stats {
        let key = format!("{}.{}", p.schema_name, p.table_name).to_ascii_lowercase();
        let e = out.entry(key.clone()).or_insert(0);
        *e = (*e).max(p.row_count);
        match p.index_id {
            Some(0) | Some(1) => { *base.entry(key).or_insert(0) += p.row_count; }
            None if p.index_name.is_none() => { *base.entry(key).or_insert(0) += p.row_count; }
            _ => {}
        }
    }
    for (k, v) in base {
        out.insert(k, v);
    }
    out
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

    // schema.table (lowercased) -> (rows, reserved KB) from partition stats, so
    // every finding below can carry the SIZE of the object it is about. This is
    // what makes findings rankable against each other: severity alone says a
    // heap is a heap, it does not say whether it holds 900 rows or 262 million.
    // Rows = MAX across partitions/indexes of the table (each index reports the
    // same row set); reserved = SUM (total space the table occupies).
    let mut size_by_table: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for p in &bundle.partition_stats {
        let e = size_by_table
            .entry(format!("{}.{}", p.schema_name, p.table_name).to_ascii_lowercase())
            .or_insert((0, 0));
        e.1 += p.reserved_kb;
    }
    for (k, rows) in table_row_counts(&bundle.partition_stats) {
        size_by_table.entry(k).or_insert((0, 0)).0 = rows;
    }
    // Build an ObjectRef, attaching measured size when we have it. Never
    // invents a size: an object missing from partition stats stays `None`,
    // which the ranking layer treats as "unknown", not as "empty".
    let obj = |schema: &str, table: &str| -> Option<ObjectRef> {
        if table.is_empty() {
            return None;
        }
        let mut o = ObjectRef::new(schema, table);
        if let Some((rows, kb)) = size_by_table.get(&o.key()) {
            o.row_count = Some(*rows);
            o.reserved_kb = Some(*kb);
        }
        Some(o)
    };

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
        // Unused index: many writes, no reads, and not the PK/clustered. A heap
        // (index_id 0) is the table itself, not an index: "0 reads, 10,370
        // writes" on a heap is an insert-only table, and `DROP INDEX [(heap)]`
        // does not compile.
        if total_reads == 0 && u.user_updates > 100 && !u.is_heap() && !u.index_name.eq_ignore_ascii_case("PK") {
            findings.push(Finding {
                rule: RuleId("dmv.unused_index".into()),
                severity: Severity::Warning,
                message: format!(
                    "{}.{} on {} has {} updates and 0 reads since last stats reset — pure write tax.",
                    u.schema_name, u.index_name, table, u.user_updates
                ),
                location: None,
                recommendation: Some("Verify the index is unused across a representative period (a server restart resets DMV counters). If confirmed, DROP it — writes amplify into every nonclustered index.".into()),
                object: obj(&u.schema_name, &u.table_name).map(|mut o| {
                    o.index = Some(u.index_name.clone());
                    o
                }),
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
                    // Exact duplicate = same key AND same INCLUDE set. Same key
                    // with different INCLUDEs is two indexes serving two
                    // different queries — an overlap to consider merging, not
                    // "one is pure overhead".
                    let same_key = same_columns(&a.key_columns, &b.key_columns);
                    let same_inc = same_key && same_column_set(&a.included_columns, &b.included_columns);
                    let (sev, verdict) = if same_inc {
                        (Severity::Error, "Keys and INCLUDE columns are identical — one of them is pure overhead.".to_string())
                    } else if same_key {
                        (Severity::Info, format!(
                            "Same key, different INCLUDE columns ({} vs {}) — overlapping indexes that each cover their own queries; consider merging into one index with the union of the INCLUDEs.",
                            fmt_cols(&a.included_columns), fmt_cols(&b.included_columns)
                        ))
                    } else {
                        (Severity::Info, "Partial overlap — review whether one can be dropped or merged with INCLUDE.".to_string())
                    };
                    findings.push(Finding {
                        rule: RuleId("dmv.duplicate_or_overlapping_index".into()),
                        severity: sev,
                        message: format!(
                            "Indexes {} and {} on {}.{} share leading key column `{}`. {}",
                            a.index_name, b.index_name, schema, table, a.key_columns[0], verdict
                        ),
                        location: None,
                        recommendation: Some("Compare INCLUDE columns and uniqueness. Keep the unique/PK one, merge needed includes into a single covering index, drop the rest.".into()),
                        object: obj(schema, table),
                    });
                }
            }
        }
    }

    // Missing indexes (DMV-suggested)
    for m in &bundle.missing_indexes {
        // A 20-row, 1-page table is read in one logical read whatever the
        // predicate; an index there is noise. Only suppress when size is KNOWN.
        if let Some((rows, _)) = size_by_table.get(&format!("{}.{}", m.schema_name, m.table_name).to_ascii_lowercase()) {
            if *rows < SMALL_TABLE_ROWS { continue; }
        }
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
            object: obj(&m.schema_name, &m.table_name),
        });
    }

    // --- Structural checks (schema-only; need NO workload and NO procs) ------
    // These fire from index metadata + row counts alone, so they surface real
    // pain on an idle, data-only database where the usage DMVs and the proc-body
    // scan find nothing. Thresholds are deliberately conservative to avoid the
    // false-positive noise that erodes DBA trust.
    {
        use std::collections::BTreeSet;
        let true_rows = table_row_counts(&bundle.partition_stats);
        let mut rows_by_table: BTreeMap<(String, String), u64> = BTreeMap::new();
        let mut heap_rows: BTreeMap<(String, String), u64> = BTreeMap::new();
        for p in &bundle.partition_stats {
            let key = (p.schema_name.clone(), p.table_name.clone());
            let rows = true_rows
                .get(&format!("{}.{}", p.schema_name, p.table_name).to_ascii_lowercase())
                .copied()
                .unwrap_or(p.row_count);
            rows_by_table.insert(key.clone(), rows);
            if p.index_name.is_none() || p.index_id == Some(0) {
                heap_rows.insert(key, rows);
            }
        }
        // Heap forward pointers measured by dm_db_index_physical_stats, when
        // the physical pass ran (it is skipped on the cheap paths).
        let forwarded: BTreeMap<(String, String), u64> = bundle
            .physical
            .iter()
            .filter(|p| p.index_id == 0)
            .filter_map(|p| p.forwarded_record_count.map(|f| ((p.schema_name.clone(), p.table_name.clone()), f)))
            .collect();
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
            let fwd = forwarded.get(&(schema.clone(), table.clone())).copied().unwrap_or(0);
            // Measured forward pointers are the proof the heap is already
            // hurting reads: every forwarded row costs an extra page read.
            let sev = if fwd >= 1000 && sev == Severity::Info { Severity::Warning } else { sev };
            let fwd_note = if fwd > 0 {
                format!(" sys.dm_db_index_physical_stats counts {} forwarded records on it — rows that grew on UPDATE and now cost an extra page read each.", commas(fwd))
            } else {
                String::new()
            };
            findings.push(Finding {
                rule: RuleId("structure.heap_table".into()),
                severity: sev,
                message: format!("{schema}.{table} is a heap (no clustered index) holding {rows} rows.{fwd_note}"),
                location: None,
                recommendation: Some("Heaps fragment over time, can't be range-seeked, and accumulate forward pointers on UPDATE that slow every read. Add a clustered index — usually the primary key, or a narrow ever-increasing surrogate. Deliberate staging / bulk-load heaps are fine, but document them.".into()),
                object: obj(schema, table),
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
                object: obj(schema, table),
            });
        }
        // Over-wide or random clustered key inflates every nonclustered index
        // (each carries the clustered key as its row locator) and, for a
        // random GUID, fragments on every insert.
        for ix in &bundle.indexes {
            // Clustered when flagged, or the PK on a non-heap table (SQL
            // Server clusters the PK by default) for bundles without the flag.
            let table_key = (ix.schema_name.clone(), ix.table_name.clone());
            let clustered = ix.is_clustered || (ix.is_primary_key && !heap_rows.contains_key(&table_key));
            if !clustered || ix.key_columns.is_empty() { continue; }
            let guid_col = ix.key_columns.iter().zip(ix.key_column_types.iter())
                .find(|(_, t)| t.eq_ignore_ascii_case("uniqueidentifier"))
                .map(|(c, _)| c.clone());
            let rows = rows_by_table.get(&table_key).copied();
            let nc_count = bundle.indexes.iter()
                .filter(|o| o.schema_name.eq_ignore_ascii_case(&ix.schema_name) && o.table_name.eq_ignore_ascii_case(&ix.table_name) && !o.is_clustered && o.index_name != ix.index_name)
                .count();
            if let Some(col) = guid_col {
                let sev = match rows { Some(r) if r >= 1_000_000 => Severity::Warning, Some(r) if r < 1000 => Severity::Info, _ => Severity::Warning };
                let newid = if ix.has_newid_default { " with a NEWID() default, so every insert lands at a random position" } else { "" };
                findings.push(Finding {
                    rule: RuleId("structure.guid_clustered_key".into()),
                    severity: sev,
                    message: format!(
                        "{}.{} is clustered on uniqueidentifier column {} ({}){}. A random 16-byte clustered key fragments the table on every insert and is copied into all {} nonclustered index(es) as the row locator, bloating each of them.",
                        ix.schema_name, ix.table_name, col, ix.index_name, newid, nc_count
                    ),
                    location: None,
                    recommendation: Some(format!(
                        "Cluster on a narrow, ever-increasing key (an IDENTITY or a (date, id) pair) and keep {} as a NONCLUSTERED UNIQUE constraint — or, if the GUID must stay clustered, use NEWSEQUENTIALID() and a lower fill factor to slow the fragmentation. Re-check sys.dm_db_index_physical_stats after: random-GUID clustered tables typically sit at 90 %+ fragmentation.",
                        col
                    )),
                    object: obj(&ix.schema_name, &ix.table_name),
                });
                continue;
            }
            let wide_cols = ix.key_columns.len() > 3;
            let wide_bytes = ix.key_bytes.map(|b| b > 16).unwrap_or(false);
            if wide_cols || wide_bytes {
                let why = match (wide_cols, ix.key_bytes) {
                    (true, Some(b)) => format!("spans {} columns, {} bytes", ix.key_columns.len(), b),
                    (true, None) => format!("spans {} columns", ix.key_columns.len()),
                    (false, Some(b)) => format!("is {} bytes wide", b),
                    (false, None) => "is wide".to_string(),
                };
                findings.push(Finding {
                    rule: RuleId("structure.wide_clustered_key".into()),
                    severity: Severity::Info,
                    message: format!(
                        "{}.{} clustered key {} {} ({}). Every nonclustered index stores the full clustered key as its row locator, so a wide key inflates all {} of them and every lookup.",
                        ix.schema_name, ix.table_name, ix.index_name, why, ix.key_columns.join(", "), nc_count
                    ),
                    location: None,
                    recommendation: Some("Keep the clustered key narrow, static, and ever-increasing — often a surrogate INT/BIGINT IDENTITY. Enforce the wide natural key with a separate UNIQUE constraint if you still need it.".into()),
                    object: obj(&ix.schema_name, &ix.table_name),
                });
            }
        }

        // Deprecated LOB column types: text / ntext / image have been
        // deprecated since SQL Server 2005 and block many operations (no
        // ORDER BY/DISTINCT, no online rebuild of the column, no string
        // functions). ALTER COLUMN to the (n)varchar(max) / varbinary(max)
        // equivalent is an in-place, metadata-plus-data change.
        for c in &bundle.deprecated_columns {
            let (replacement, sev) = match c.type_name.to_ascii_lowercase().as_str() {
                "text" => ("varchar(max)", Severity::Warning),
                "ntext" => ("nvarchar(max)", Severity::Warning),
                "image" => ("varbinary(max)", Severity::Warning),
                _ => continue,
            };
            let rows = rows_by_table.get(&(c.schema_name.clone(), c.table_name.clone())).copied();
            findings.push(Finding {
                rule: RuleId("structure.deprecated_lob_type".into()),
                severity: sev,
                message: format!(
                    "{}.{}.{} is declared {} — a deprecated LOB type (since SQL Server 2005) that string functions, ORDER BY/DISTINCT and many tools cannot use.{}",
                    c.schema_name, c.table_name, c.column_name, c.type_name,
                    rows.map(|r| format!(" ({} rows)", commas(r))).unwrap_or_default()
                ),
                location: None,
                recommendation: Some(format!(
                    "Convert in place (rewrites the LOB data; run in a maintenance window on large tables):\n\n{}",
                    deprecated_column_ddl(c, replacement)
                )),
                object: obj(&c.schema_name, &c.table_name),
            });
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

/// Tables with fewer rows than this get no missing-index / table-scan advice
/// from the connected path: a one-page table is one logical read however it
/// is accessed, and an index on it is pure maintenance cost.
pub const SMALL_TABLE_ROWS: u64 = 1_000;

fn same_columns(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| unbr(x).eq_ignore_ascii_case(&unbr(y)))
}

/// Order-insensitive column-set equality (INCLUDE order is irrelevant).
fn same_column_set(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| unbr(x).eq_ignore_ascii_case(&unbr(y))))
}

fn fmt_cols(cols: &[String]) -> String {
    if cols.is_empty() { "none".to_string() } else { cols.join(", ") }
}

/// `ALTER TABLE … ALTER COLUMN … <replacement>` for a deprecated LOB column.
pub fn deprecated_column_ddl(c: &DeprecatedColumn, replacement: &str) -> String {
    format!(
        "ALTER TABLE {}.{} ALTER COLUMN {} {} {};",
        br(&c.schema_name), br(&c.table_name), br(&c.column_name), replacement,
        if c.is_nullable { "NULL" } else { "NOT NULL" }
    )
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
    /// Rebuild/reorganize a fragmented or under-filled index (physical stats).
    RebuildIndex,
    /// Refresh stale statistics (`dm_db_stats_properties`).
    UpdateStatistics,
    /// Convert a deprecated text/ntext/image column to its (n)varchar(max) twin.
    AlterColumnType,
    /// Parameter-sniffing skew seen in Query Store: same query, ≥ 2 plans, cost swings ≥ 10×.
    ParameterSniffing,
}

impl RecKind {
    /// Kinds whose evidence is a `sys.dm_db_*_stats` usage counter that resets
    /// on restart (and therefore carry the counter-age stamp). Physical,
    /// statistics, column-type and Query-Store recs are measured directly.
    pub fn is_usage_based(self) -> bool {
        matches!(self, RecKind::CreateIndex | RecKind::DropIndex | RecKind::MergeIndex | RecKind::ColumnstoreCandidate)
    }
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
    use std::collections::BTreeSet;
    let mut recs: Vec<Recommendation> = Vec::new();

    // ---- CreateIndex: from SQL Server's own missing-index DMV --------------
    // Cross-group consolidation first: the DMV emits one suggestion per query
    // shape, so the SAME table often gets several near-identical suggestions
    // that differ only by a trailing key column or an INCLUDE. Left un-merged
    // they'd become N overlapping indexes (write amplification + the very
    // duplication this advisor is meant to prevent). `consolidate_missing_indexes`
    // folds prefix-related suggestions into one superset index (widest key,
    // unioned INCLUDEs) before we emit any DDL. See its docs for the merge rule.
    let consolidated = consolidate_missing_indexes_traced(&bundle.missing_indexes);
    let true_rows = table_row_counts(&bundle.partition_stats);
    let young_counters = bundle.counter_age_secs.map(|a| a < COUNTER_AGE_YOUNG_SECS).unwrap_or(false);
    // Names already taken on each table, and the catalog rows themselves, so
    // a CREATE INDEX never collides with an existing name and a suggestion that
    // merely WIDENS an existing key becomes a DROP_EXISTING rebuild of it.
    let mut taken_names: BTreeSet<(String, String)> = BTreeSet::new();
    for ix in &bundle.indexes {
        taken_names.insert((format!("{}.{}", ix.schema_name, ix.table_name).to_ascii_lowercase(), ix.index_name.to_ascii_lowercase()));
    }
    for (m, seek_parts) in &consolidated {
        let keys: Vec<String> = m.equality_columns.iter().chain(m.inequality_columns.iter()).cloned().collect();
        if keys.is_empty() { continue; }
        let table_lc = format!("{}.{}", m.schema_name, m.table_name).to_ascii_lowercase();
        // A table under SMALL_TABLE_ROWS is one page: no index will change
        // its cost, whatever the DMV's impact score says. Only when measured.
        if let Some(rows) = true_rows.get(&table_lc) {
            if *rows < SMALL_TABLE_ROWS { continue; }
        }
        // Existing index on the same table that this suggestion widens or
        // duplicates: same key (exactly), non-constraint.
        let existing_same_key = bundle.indexes.iter().find(|ix| {
            ix.schema_name.eq_ignore_ascii_case(&m.schema_name)
                && ix.table_name.eq_ignore_ascii_case(&m.table_name)
                && !ix.is_primary_key && !ix.is_unique && !ix.is_clustered
                && same_columns(&ix.key_columns, &keys)
        });
        if let Some(ix) = existing_same_key {
            // Already covered: the existing index has this key and every
            // requested INCLUDE. The DMV row predates it or the plan is cached;
            // there is nothing to create.
            if m.included_columns.iter().all(|c| ix.included_columns.iter().chain(ix.key_columns.iter()).any(|e| col_eq(e, c))) {
                continue;
            }
        }
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
        // Workload phrase: a per-day rate is only honest once a full day of
        // Query-Store capture AND a full day of counters exist. Under that,
        // say what was measured ("2,062 executions in 0.9 h of capture") —
        // never "×/day" extrapolated from fifty minutes.
        let busiest = crate::advisor_workload::busiest_for(&bundle.workload, &m.schema_name, &m.table_name);
        let (workload_phrase, runs_chip): (Option<String>, Option<(String, String)>) = match busiest {
            Some(w) if w.execution_count > 0 => {
                let extrapolate = w.window_hours >= 24.0 && !young_counters;
                if extrapolate {
                    let per_day = w.executions_per_day();
                    let rendered = if per_day >= 1.0 { format!("{:.0}", per_day.round()) } else { format!("{per_day:.1}") };
                    (
                        Some(format!("helps the busiest query on this table, which runs ~{rendered}×/day ({} executions over {:.0} h of Query Store capture)", commas(w.execution_count), w.window_hours)),
                        Some(("Query runs".into(), format!("~{rendered}×/day (busiest query on table)"))),
                    )
                } else {
                    (
                        Some(format!("helps the busiest query on this table: {} executions in {:.1} h of Query Store capture (too short to project a daily rate)", commas(w.execution_count), w.window_hours)),
                        Some(("Query runs".into(), format!("{} in {:.1} h of capture (busiest query on table)", commas(w.execution_count), w.window_hours))),
                    )
                }
            }
            _ => (None, None),
        };
        let key_list = keys.iter().map(|c| br(c)).collect::<Vec<_>>().join(", ");
        // Widening an existing same-key index: union its INCLUDEs with the
        // request and rebuild it in place under its own name.
        let (name, widen_of, inc_cols): (String, Option<&IndexMeta>, Vec<String>) = match existing_same_key {
            Some(ix) => {
                let mut inc = ix.included_columns.clone();
                union_includes(&mut inc, &m.included_columns);
                (ix.index_name.clone(), Some(ix), inc)
            }
            None => {
                let mut name = format!("IX_{}_{}", idfrag(&m.table_name), keys.iter().map(|c| idfrag(c)).collect::<Vec<_>>().join("_"));
                name.truncate(120);
                // Never propose a name the table already uses (the CREATE
                // would fail with "already exists"): suffix until free.
                let base = name.clone();
                let mut n = 2;
                while taken_names.contains(&(table_lc.clone(), name.to_ascii_lowercase())) {
                    name = format!("{base}_{n}");
                    n += 1;
                }
                (name, None, m.included_columns.clone())
            }
        };
        taken_names.insert((table_lc.clone(), name.to_ascii_lowercase()));
        let inc_list = inc_cols.iter().map(|c| br(c)).collect::<Vec<_>>().join(", ");
        let inc_clause = if inc_cols.is_empty() { String::new() } else { format!("\n  INCLUDE ({})", inc_list) };
        let ddl = match widen_of {
            Some(ix) => format!(
                "-- {} already exists on ({}); this rebuilds it in place with the extra INCLUDE columns.\nCREATE NONCLUSTERED INDEX [{}]\n  ON {}.{} ({}){}\n  WITH (DROP_EXISTING = ON);",
                ix.index_name, ix.key_columns.join(", "), name, br(&m.schema_name), br(&m.table_name), key_list, inc_clause
            ),
            None => format!(
                "CREATE NONCLUSTERED INDEX [{}]\n  ON {}.{} ({}){};",
                name, br(&m.schema_name), br(&m.table_name), key_list, inc_clause
            ),
        };
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
        // Persistence across the monitor's daily DMV snapshots, when present.
        // The DMV forgets on every restart; "seen on 6 of the last 7 days" is
        // the evidence that turns one reading into a pattern.
        let history = bundle
            .missing_index_history
            .iter()
            .find(|h| h.schema_name.eq_ignore_ascii_case(&m.schema_name) && h.table_name.eq_ignore_ascii_case(&m.table_name));
        let history_note = match history {
            Some(h) => format!(
                " The monitor has seen a missing-index suggestion on this table on {} of the last {} monitored day(s).",
                h.days_seen, h.days_observed
            ),
            None => String::new(),
        };
        let merged_note = if seek_parts.len() > 1 {
            format!(
                " ({} DMV groups with the same key merged into this one index; seeks summed: {}.)",
                seek_parts.len(),
                seek_parts.iter().map(|n| commas(*n)).collect::<Vec<_>>().join(" + ")
            )
        } else {
            String::new()
        };
        let seeks_phrase = match bundle.counter_age_secs {
            Some(age) => format!("{} seeks in {:.1} h since restart would benefit", commas(m.user_seeks), age.max(0) as f64 / 3600.0),
            None => format!("{} seeks would benefit", commas(m.user_seeks)),
        };
        let widen_note = match widen_of {
            Some(ix) => format!(" {} already exists with this key ({}) but not these INCLUDE columns — widen it in place rather than adding a second index on the same key.", ix.index_name, ix.key_columns.join(", ")),
            None => String::new(),
        };
        let rationale = format!(
            "SQL Server's missing-index DMV: {}, avg query cost {:.2}, estimated improvement {:.0}%.{}{}{}{}{} Order key columns by selectivity and consolidate with existing indexes before applying.",
            seeks_phrase, m.avg_total_user_cost, m.avg_user_impact, merged_note, widen_note, write_cost_note, workload_note, history_note
        );
        let mut metrics = vec![
            ("Seeks that benefit".into(), commas(m.user_seeks)),
            ("Est. cost reduction".into(), format!("~{:.0}%", m.avg_user_impact)),
            ("Avg query cost".into(), format!("{:.2}", m.avg_total_user_cost)),
            ("Write cost".into(), write_cost.label().to_string()),
        ];
        if seek_parts.len() > 1 {
            metrics.push(("DMV groups merged".into(), format!("{}", seek_parts.len())));
        }
        if let Some(h) = history {
            metrics.push(("Seen (days)".into(), format!("{} of {}", h.days_seen, h.days_observed)));
        }
        if let Some(chip) = runs_chip {
            metrics.push(chip);
        }
        let title = match widen_of {
            Some(ix) => format!("Widen {} on {}.{} to cover ({})", ix.index_name, m.schema_name, m.table_name, inc_cols.join(", ")),
            None => format!("Create covering index on {}.{}", m.schema_name, m.table_name),
        };
        recs.push(Recommendation {
            kind: RecKind::CreateIndex,
            priority: priority.into(),
            title,
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
    // Per-index usage counters so a merge/drop rec can report what was
    // actually measured for the index it proposes to drop. A missing row
    // means sys.dm_db_index_usage_stats has not seen the index since the
    // last restart — which is NOT the same as "zero reads".
    let mut usage_by_index: BTreeMap<(String, String, String), &IndexUsage> = BTreeMap::new();
    for u in &bundle.index_usage {
        usage_by_index.insert(
            (u.schema_name.to_lowercase(), u.table_name.to_lowercase(), u.index_name.to_lowercase()),
            u,
        );
    }
    // (Reads chip, Writes chip, confidence) for the index being dropped.
    let usage_chips = |schema: &str, table: &str, index: &str| -> (Vec<(String, String)>, &'static str) {
        match usage_by_index.get(&(schema.to_lowercase(), table.to_lowercase(), index.to_lowercase())) {
            Some(u) if !u.no_stats_row => (
                vec![
                    ("Reads".into(), commas(u.user_seeks + u.user_scans + u.user_lookups)),
                    ("Writes".into(), commas(u.user_updates)),
                ],
                "observed",
            ),
            _ => (vec![("Reads".into(), "no usage recorded since restart".into())], "estimated"),
        }
    };
    let age_hours = bundle.counter_age_secs.map(|a| a.max(0) as f64 / 3600.0);
    let history_for = |schema: &str, table: &str, index: &str| -> Option<&IndexUsageHistory> {
        bundle.index_usage_history.iter().find(|h| {
            h.schema_name.eq_ignore_ascii_case(schema) && h.table_name.eq_ignore_ascii_case(table) && h.index_name.eq_ignore_ascii_case(index)
        })
    };
    for u in &bundle.index_usage {
        let reads = u.user_seeks + u.user_scans + u.user_lookups;
        if reads != 0 { continue; }
        // A heap is the table, not an index — never a drop candidate, and
        // `DROP INDEX [(heap)]` does not compile.
        if u.is_heap() { continue; }
        let key = (u.schema_name.to_lowercase(), u.table_name.to_lowercase(), u.index_name.to_lowercase());
        // Never drop a PK, unique constraint, or clustered index, or an obvious PK by name.
        if let Some(ix) = meta.get(&key) {
            if ix.is_primary_key || ix.is_unique || ix.is_clustered { continue; }
        }
        if u.index_name.to_lowercase().starts_with("pk_") || u.index_name.eq_ignore_ascii_case("PK") { continue; }
        let reserved_kb = reserved_by_index.get(&key).copied().unwrap_or(0);
        let hist = history_for(&u.schema_name, &u.table_name, &u.index_name);
        // Monitor-backed silence: watched ≥ 7 days and never read on any of them.
        let idle_days = hist.filter(|h| h.days_observed >= 7 && h.days_with_reads == 0).map(|h| h.days_observed);
        let since = match age_hours {
            Some(h) => format!("since restart ({h:.1} h)"),
            None => "since the last counter reset".to_string(),
        };
        if u.user_updates > 100 {
            // Write-only: pays on every write, never read.
            let mut priority = if u.user_updates >= 100_000 { "high" } else if u.user_updates >= 10_000 { "medium" } else { "low" };
            if young_counters && priority == "high" { priority = "medium"; }
            let title = if young_counters {
                format!("Review write-only index {} on {}.{} (0 reads {since})", u.index_name, u.schema_name, u.table_name)
            } else {
                format!("Drop unused index {} on {}.{}", u.index_name, u.schema_name, u.table_name)
            };
            let idle_note = match idle_days {
                Some(d) => format!(" The monitor has watched it for {d} days without a single read."),
                None => String::new(),
            };
            recs.push(Recommendation {
                kind: RecKind::DropIndex,
                priority: priority.into(),
                title,
                object: format!("{}.{}.{}", u.schema_name, u.table_name, u.index_name),
                rationale: format!(
                    "{} updates and 0 reads {since} — the index is pure write tax (every INSERT/UPDATE/DELETE maintains it for no read benefit).{} Confirm across a representative window first (DMV counters reset on restart).",
                    commas(u.user_updates), idle_note
                ),
                ddl: drop_index_ddl(&u.schema_name, &u.table_name, &u.index_name, young_counters, age_hours),
                impact_score: u.user_updates as f64,
                metrics: vec![
                    ("Writes maintained".into(), commas(u.user_updates)),
                    ("Reads".into(), "0".into()),
                    ("Storage reclaimed".into(), format!("~{} MB", kb_to_mb(reserved_kb))),
                ],
                // Counters are measured directly from sys.dm_db_index_usage_stats.
                confidence: "observed".into(),
            });
        } else if u.user_updates == 0 {
            // Never touched at all: not read, not written. On an idle table
            // that is expected; the signal is "nothing has asked for this
            // index since the counters began". Low until the monitor has
            // watched it idle for a week.
            let priority = if idle_days.is_some() { "medium" } else { "low" };
            let observed = if u.no_stats_row {
                format!("No usage row exists for it in sys.dm_db_index_usage_stats {since}: no seek, scan, lookup or write has touched it")
            } else {
                format!("0 reads and 0 writes {since}")
            };
            let idle_note = match idle_days {
                Some(d) => format!(" The monitor has watched it for {d} days without a read — promoted to medium."),
                None if young_counters => " Under 24 h of counters is a sample, not a verdict: re-check after a representative window (or let the monitor watch it for a week).".to_string(),
                None => String::new(),
            };
            recs.push(Recommendation {
                kind: RecKind::DropIndex,
                priority: priority.into(),
                title: format!("Index {} on {}.{} never used {since}", u.index_name, u.schema_name, u.table_name),
                object: format!("{}.{}.{}", u.schema_name, u.table_name, u.index_name),
                rationale: format!(
                    "{}. It still occupies ~{} MB and will be maintained by the first write that arrives.{}",
                    observed, kb_to_mb(reserved_kb), idle_note
                ),
                ddl: drop_index_ddl(&u.schema_name, &u.table_name, &u.index_name, young_counters && idle_days.is_none(), age_hours),
                impact_score: reserved_kb as f64 / 1024.0,
                metrics: {
                    let mut m = vec![
                        ("Reads".into(), "0".into()),
                        ("Writes".into(), "0".into()),
                        ("Storage".into(), format!("~{} MB", kb_to_mb(reserved_kb))),
                    ];
                    if let Some(h) = hist {
                        m.push(("Monitored (days)".into(), format!("{} read on {} of {}", if h.days_with_reads == 0 { "never" } else { "read" }, h.days_with_reads, h.days_observed)));
                    }
                    m
                },
                confidence: if young_counters { "estimated".into() } else { "observed".into() },
            });
        }
    }

    // ---- MergeIndex: exact-duplicate key columns on the same table ---------
    let mut by_table: BTreeMap<(String, String), Vec<&IndexMeta>> = BTreeMap::new();
    for ix in &bundle.indexes {
        by_table.entry((ix.schema_name.clone(), ix.table_name.clone())).or_default().push(ix);
    }
    let reads_of = |schema: &str, table: &str, index: &str| -> Option<u64> {
        usage_by_index
            .get(&(schema.to_lowercase(), table.to_lowercase(), index.to_lowercase()))
            .filter(|u| !u.no_stats_row)
            .map(|u| u.user_seeks + u.user_scans + u.user_lookups)
    };
    for ((schema, table), list) in &by_table {
        for i in 0..list.len() {
            for j in (i + 1)..list.len() {
                let (a, b) = (list[i], list[j]);
                let same_key = !a.key_columns.is_empty() && same_columns(&a.key_columns, &b.key_columns);
                if !same_key { continue; }
                let same_inc = same_column_set(&a.included_columns, &b.included_columns);
                if same_inc {
                    // Exact duplicate: keep the unique/PK one; drop the other.
                    let (keep, drop) = if a.is_primary_key || a.is_unique { (a, b) }
                        else if b.is_primary_key || b.is_unique { (b, a) }
                        else { (a, b) };
                    if drop.is_primary_key || drop.is_unique { continue; }
                    let drop_reserved_kb = reserved_by_index
                        .get(&(schema.to_lowercase(), table.to_lowercase(), drop.index_name.to_lowercase()))
                        .copied()
                        .unwrap_or(0);
                    let (drop_chips, drop_conf) = usage_chips(schema, table, &drop.index_name);
                    recs.push(Recommendation {
                        kind: RecKind::MergeIndex,
                        priority: merge_priority(drop_reserved_kb).into(),
                        title: format!("Merge duplicate indexes on {}.{}", schema, table),
                        object: format!("{}.{}.{}", schema, table, drop.index_name),
                        rationale: format!(
                            "Indexes {} and {} have identical key columns ({}) and identical INCLUDE columns ({}). One is redundant — every write maintains both. Drop {}.",
                            keep.index_name, drop.index_name, drop.key_columns.join(", "), fmt_cols(&drop.included_columns), drop.index_name
                        ),
                        ddl: format!(
                            "-- {} is an exact duplicate of {} (same key, same INCLUDE):\nDROP INDEX {} ON {}.{};",
                            drop.index_name, keep.index_name, br(&drop.index_name), br(schema), br(table)
                        ),
                        // Rank within kind by what the drop reclaims (+1 so an
                        // exact duplicate edges a prefix-redundant one of equal size).
                        impact_score: drop_reserved_kb as f64 + 1.0,
                        metrics: {
                            let mut m = vec![("Storage".into(), format!("~{} MB", kb_to_mb(drop_reserved_kb)))];
                            m.extend(drop_chips);
                            m
                        },
                        confidence: drop_conf.into(),
                    });
                    continue;
                }
                // Same key, different INCLUDEs: two indexes each covering their
                // own queries. Not a duplicate — an overlap worth merging into
                // one index with the union of INCLUDEs. The index to retire is
                // the one with FEWER reads; never the one doing the work.
                if a.is_primary_key || a.is_unique || b.is_primary_key || b.is_unique { continue; }
                let ra = reads_of(schema, table, &a.index_name);
                let rb = reads_of(schema, table, &b.index_name);
                let (keep, drop) = match (ra, rb) {
                    (Some(x), Some(y)) if y > x => (b, a),
                    (None, Some(y)) if y > 0 => (b, a),
                    _ => (a, b),
                };
                let mut inc = keep.included_columns.clone();
                union_includes(&mut inc, &drop.included_columns);
                let inc_list = inc.iter().map(|c| br(c)).collect::<Vec<_>>().join(", ");
                let key_list = keep.key_columns.iter().map(|c| br(c)).collect::<Vec<_>>().join(", ");
                let drop_reserved_kb = reserved_by_index
                    .get(&(schema.to_lowercase(), table.to_lowercase(), drop.index_name.to_lowercase()))
                    .copied()
                    .unwrap_or(0);
                let reads_note = |name: &str, r: Option<u64>| match r {
                    Some(n) => format!("{name}: {} reads", commas(n)),
                    None => format!("{name}: no usage recorded since restart"),
                };
                let (keep_reads, drop_reads) = if std::ptr::eq(keep, a) { (ra, rb) } else { (rb, ra) };
                recs.push(Recommendation {
                    kind: RecKind::MergeIndex,
                    priority: "low".into(),
                    title: format!("Overlapping indexes {} and {} on {}.{} — consider merging", keep.index_name, drop.index_name, schema, table),
                    object: format!("{}.{}.{}", schema, table, drop.index_name),
                    rationale: format!(
                        "{} and {} share the key ({}) but carry different INCLUDE columns ({} vs {}), so each serves its own queries — neither is redundant today ({}; {}). Merging them into one index with the union of INCLUDEs halves the write maintenance on this key at the cost of a slightly wider index. Low priority: only worth doing if {}.{} is write-heavy.",
                        keep.index_name, drop.index_name, keep.key_columns.join(", "),
                        fmt_cols(&keep.included_columns), fmt_cols(&drop.included_columns),
                        reads_note(&keep.index_name, keep_reads), reads_note(&drop.index_name, drop_reads),
                        schema, table
                    ),
                    ddl: format!(
                        "-- Optional merge: widen {} to carry both INCLUDE sets, then retire {} (the one with fewer reads).\nCREATE NONCLUSTERED INDEX {} ON {}.{} ({})\n  INCLUDE ({})\n  WITH (DROP_EXISTING = ON);\nDROP INDEX {} ON {}.{};",
                        keep.index_name, drop.index_name,
                        br(&keep.index_name), br(schema), br(table), key_list, inc_list,
                        br(&drop.index_name), br(schema), br(table)
                    ),
                    impact_score: drop_reserved_kb as f64,
                    metrics: vec![
                        ("Storage".into(), format!("~{} MB", kb_to_mb(drop_reserved_kb))),
                        (format!("Reads {}", keep.index_name), keep_reads.map(commas).unwrap_or_else(|| "none recorded".into())),
                        (format!("Reads {}", drop.index_name), drop_reads.map(commas).unwrap_or_else(|| "none recorded".into())),
                    ],
                    confidence: if keep_reads.is_some() && drop_reads.is_some() { "observed".into() } else { "estimated".into() },
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
    // the classic "borderline duplicate" shape. We never
    // touch a PK/unique index (it enforces a constraint), and `redundant_existing_indexes`
    // dedupes so a key list that's a prefix of several survivors is only flagged once.
    for r in redundant_existing_indexes(&bundle.indexes) {
        let drop_reserved_kb = reserved_by_index
            .get(&(r.schema.to_lowercase(), r.table.to_lowercase(), r.redundant_index.to_lowercase()))
            .copied()
            .unwrap_or(0);
        let (drop_chips, drop_conf) = usage_chips(&r.schema, &r.table, &r.redundant_index);
        recs.push(Recommendation {
            kind: RecKind::MergeIndex,
            priority: merge_priority(drop_reserved_kb).into(),
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
            impact_score: drop_reserved_kb as f64,
            metrics: {
                let mut m = vec![("Storage".into(), format!("~{} MB", kb_to_mb(drop_reserved_kb)))];
                m.extend(drop_chips);
                m
            },
            // Prefix relationship is read directly from catalog key-column metadata;
            // the Reads/Writes chips are only "observed" when a usage row exists.
            confidence: drop_conf.into(),
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
    // Which tables are heaps (a partition row with no index name = index_id 0).
    let heap_tables: std::collections::BTreeSet<(String, String)> = bundle
        .partition_stats
        .iter()
        .filter(|p| p.index_name.is_none())
        .map(|p| (p.schema_name.to_lowercase(), p.table_name.to_lowercase()))
        .collect();
    /// Minimum scan count before a table is called "scan-dominated". Usage
    /// counters reset on every restart, so a handful of scans (a DBA's own
    /// test runs minutes after a reboot) must not produce the #1-ranked rec.
    const COLUMNSTORE_MIN_SCANS: u64 = 100;
    for ((schema, table), (rows, reserved_kb)) in &size_by_table {
        let (seeks, scans, updates) = usage_by_table.get(&(schema.clone(), table.clone())).copied().unwrap_or((0, 0, 0));
        let reads = seeks + scans;
        // Large, scan-dominated, low write churn → analytic table that wants a CCI.
        let big = *rows >= 1_000_000 || *reserved_kb >= 1_048_576; // ≥1M rows or ≥1GB
        let scan_heavy = scans > seeks && scans >= COLUMNSTORE_MIN_SCANS;
        let low_churn = reads == 0 || (updates as f64) < (reads as f64) * 0.2;
        if big && scan_heavy && low_churn {
            let priority = if *reserved_kb >= 1_048_576 { "high" } else { "medium" };
            let is_heap = heap_tables.contains(&(schema.to_lowercase(), table.to_lowercase()));
            let clustered = clustered_index_for(&bundle.indexes, schema, table, is_heap);
            recs.push(Recommendation {
                kind: RecKind::ColumnstoreCandidate,
                priority: priority.into(),
                title: format!("Columnstore candidate: {}.{}", schema, table),
                object: format!("{}.{}", schema, table),
                rationale: format!(
                    "{} rows, {:.0} MB, scan-dominated ({} scans vs {} seeks) with low write churn ({} updates). Analytic/scan workloads on tables this size typically get 5–10× compression and large scan speedups from a clustered columnstore index. Usage counters reset on restart — {} scans is a small sample; confirm over a representative window first.",
                    rows, (*reserved_kb as f64) / 1024.0, scans, seeks, updates, commas(scans)
                ),
                ddl: columnstore_ddl(schema, table, is_heap, clustered),
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

    recs.extend(physical_recs(bundle));
    recs.extend(stats_recs(bundle));
    recs.extend(deprecated_column_recs(bundle));
    recs.extend(sniffing_recs(bundle));

    // Every usage-based rec above is built from counters that only exist since
    // the last restart — say so on each one, and don't call a ten-minute
    // sample "observed".
    apply_counter_age(&mut recs, bundle.counter_age_secs, bundle.counters_since.as_deref());

    // Rank: priority bucket first, then impact within the bucket.
    recs.sort_by(|a, b| {
        priority_rank(&a.priority).cmp(&priority_rank(&b.priority))
            .then(b.impact_score.partial_cmp(&a.impact_score).unwrap_or(std::cmp::Ordering::Equal))
    });
    recs
}

/// `DROP INDEX` DDL for a usage-based drop rec. Under 24 h of counters the
/// statement is shipped behind a comment that says why it must not be run
/// yet — a ten-minute "0 reads" is a sample, and the copy-paste promise cuts
/// both ways.
fn drop_index_ddl(schema: &str, table: &str, index: &str, hold: bool, age_hours: Option<f64>) -> String {
    let stmt = format!("DROP INDEX {} ON {}.{};", br(index), br(schema), br(table));
    if hold {
        let age = age_hours.map(|h| format!("{h:.1} h")).unwrap_or_else(|| "under 24 h".to_string());
        format!(
            "-- NOT YET: usage counters cover only {age} since restart. Re-check after ≥ 24 h (or a monitored week) before running:\n-- {stmt}"
        )
    } else {
        stmt
    }
}

/// Physical-shape recs from `sys.dm_db_index_physical_stats`:
///   * ≥ 30 % fragmentation on ≥ 1,000 leaf pages → REBUILD (≥ 30 %) / REORGANIZE (10–30 % is NOT flagged — below the rebuild threshold it is noise on modern storage);
///   * fill factor < 70 on an index that is read far more than written → rebuild at 90–100.
/// Heap forward pointers are folded into the heap finding in `analyze`, not repeated here.
pub fn physical_recs(bundle: &DmvBundle) -> Vec<Recommendation> {
    const FRAG_PCT: f64 = 30.0;
    const MIN_PAGES: u64 = 1_000;
    let mut out = Vec::new();
    let mut usage: BTreeMap<(String, String, String), &IndexUsage> = BTreeMap::new();
    for u in &bundle.index_usage {
        usage.insert((u.schema_name.to_lowercase(), u.table_name.to_lowercase(), u.index_name.to_lowercase()), u);
    }
    for p in &bundle.physical {
        let Some(index_name) = &p.index_name else { continue };
        if p.index_id <= 0 || p.page_count < MIN_PAGES { continue; }
        let key = (p.schema_name.to_lowercase(), p.table_name.to_lowercase(), index_name.to_lowercase());
        let u = usage.get(&key).filter(|u| !u.no_stats_row);
        let reads = u.map(|u| u.user_seeks + u.user_scans + u.user_lookups);
        let writes = u.map(|u| u.user_updates);
        let mb = p.page_count as f64 * 8.0 / 1024.0;
        if p.avg_fragmentation_pct >= FRAG_PCT {
            let priority = if p.avg_fragmentation_pct >= 70.0 && p.page_count >= 10_000 { "high" } else if p.page_count >= 10_000 { "medium" } else { "low" };
            let density = p.avg_page_space_used_pct.map(|d| format!(", {d:.0} % page density")).unwrap_or_default();
            out.push(Recommendation {
                kind: RecKind::RebuildIndex,
                priority: priority.into(),
                title: format!("Rebuild {} on {}.{} ({:.0} % fragmented, {} pages)", index_name, p.schema_name, p.table_name, p.avg_fragmentation_pct, commas(p.page_count)),
                object: format!("{}.{}.{}", p.schema_name, p.table_name, index_name),
                rationale: format!(
                    "sys.dm_db_index_physical_stats (LIMITED) measures {:.1} % logical fragmentation across {} leaf pages (~{:.0} MB){}. Range scans and read-ahead on this index touch pages out of order and read more of them than the data needs; a rebuild puts the leaf level back in key order and reclaims the half-empty pages. If the key is a random GUID or the table is hot-inserted, expect it to fragment again — fix the key or use a lower fill factor.",
                    p.avg_fragmentation_pct, commas(p.page_count), mb, density
                ),
                ddl: format!(
                    "-- Online when the edition allows it; otherwise drop ONLINE = ON.\nALTER INDEX {} ON {}.{} REBUILD WITH (ONLINE = ON, SORT_IN_TEMPDB = ON);",
                    br(index_name), br(&p.schema_name), br(&p.table_name)
                ),
                impact_score: p.avg_fragmentation_pct * p.page_count as f64 / 100.0,
                metrics: vec![
                    ("Fragmentation".into(), format!("{:.1} %", p.avg_fragmentation_pct)),
                    ("Leaf pages".into(), commas(p.page_count)),
                    ("Size".into(), format!("~{mb:.0} MB")),
                ],
                confidence: "observed".into(),
            });
        }
        // Low fill factor on a read-mostly index: pages are deliberately kept
        // part-empty for inserts that never come, so every scan reads ~2×.
        if p.fill_factor > 0 && p.fill_factor < 70 {
            let read_mostly = match (reads, writes) {
                (Some(r), Some(w)) => r > 0 && w * 10 <= r,
                _ => false,
            };
            if read_mostly {
                let density = p.avg_page_space_used_pct.map(|d| format!(" (measured page density {d:.0} %)")).unwrap_or_default();
                out.push(Recommendation {
                    kind: RecKind::RebuildIndex,
                    priority: (if p.page_count >= 10_000 { "medium" } else { "low" }).into(),
                    title: format!("Raise fill factor on {} ({}) on {}.{} — read-mostly index", index_name, p.fill_factor, p.schema_name, p.table_name),
                    object: format!("{}.{}.{}", p.schema_name, p.table_name, index_name),
                    rationale: format!(
                        "FILLFACTOR = {} leaves every leaf page {} % empty{}, so the index occupies ~{} pages (~{:.0} MB) where ~{} would hold the same rows. Usage since restart is {} reads vs {} writes — the free space is reserved for inserts that are not arriving. Rebuild at 90–100 and let the next measurement confirm the page count roughly halves.",
                        p.fill_factor, 100 - p.fill_factor as u32, density, commas(p.page_count), mb,
                        commas((p.page_count as f64 * p.fill_factor as f64 / 100.0).round() as u64),
                        reads.map(commas).unwrap_or_default(), writes.map(commas).unwrap_or_default()
                    ),
                    ddl: format!(
                        "ALTER INDEX {} ON {}.{} REBUILD WITH (FILLFACTOR = 100, ONLINE = ON, SORT_IN_TEMPDB = ON);",
                        br(index_name), br(&p.schema_name), br(&p.table_name)
                    ),
                    impact_score: (100 - p.fill_factor as u32) as f64 * p.page_count as f64 / 100.0,
                    metrics: vec![
                        ("Fill factor".into(), format!("{}", p.fill_factor)),
                        ("Leaf pages".into(), commas(p.page_count)),
                        ("Reads / writes".into(), format!("{} / {}", reads.map(commas).unwrap_or_default(), writes.map(commas).unwrap_or_default())),
                    ],
                    confidence: "observed".into(),
                });
            }
        }
    }
    out
}

/// Stale-statistics recs from `sys.dm_db_stats_properties`:
///   * `no_recompute = 1` with modifications ≥ 10 % of the histogram's rows —
///     auto-update is OFF, so nothing will ever refresh it;
///   * modifications ≥ 20 % of rows on a ≥ 100k-row table — large enough that
///     the auto-update threshold lagging costs real estimate error (small
///     tables auto-refresh on the next compile; not worth a card).
pub fn stats_recs(bundle: &DmvBundle) -> Vec<Recommendation> {
    let mut out = Vec::new();
    for st in &bundle.stats {
        if st.rows == 0 { continue; }
        let pct = st.modification_counter as f64 * 100.0 / st.rows as f64;
        let norecompute_stale = st.no_recompute && pct >= 10.0 && st.modification_counter >= 1_000;
        let big_stale = !st.no_recompute && st.rows >= 100_000 && pct >= 20.0;
        if !norecompute_stale && !big_stale { continue; }
        let current = st.table_rows.map(|r| format!(" The table now holds {} rows.", commas(r))).unwrap_or_default();
        let priority = if st.no_recompute && pct >= 50.0 { "high" } else if st.no_recompute || pct >= 50.0 { "medium" } else { "low" };
        let norecompute_ddl = if st.no_recompute {
            if st.is_index_stat {
                format!(
                    "\n-- Re-enable auto-update on the index statistics (NORECOMPUTE was ON):\nALTER INDEX {} ON {}.{} SET (STATISTICS_NORECOMPUTE = OFF);",
                    br(&st.stats_name), br(&st.schema_name), br(&st.table_name)
                )
            } else {
                "\n-- Running UPDATE STATISTICS without NORECOMPUTE re-enables auto-update for this statistic.".to_string()
            }
        } else {
            String::new()
        };
        out.push(Recommendation {
            kind: RecKind::UpdateStatistics,
            priority: priority.into(),
            title: format!(
                "Refresh stale statistics {} on {}.{} ({} modifications vs {} rows{})",
                st.stats_name, st.schema_name, st.table_name, commas(st.modification_counter), commas(st.rows),
                if st.no_recompute { ", auto-update OFF" } else { "" }
            ),
            object: format!("{}.{}.{}", st.schema_name, st.table_name, st.stats_name),
            rationale: format!(
                "sys.dm_db_stats_properties: the histogram was built from {} rows ({} sampled){} and {} modifications ({:.0} %) have landed since.{} The optimizer estimates row counts from this histogram, so date ranges and values it never saw get the density fallback — wrong join orders, wrong memory grants, spills. Refresh with FULLSCAN{}.",
                commas(st.rows), commas(st.rows_sampled),
                st.last_updated.as_deref().map(|d| format!(" on {d}")).unwrap_or_default(),
                commas(st.modification_counter), pct, current,
                if st.no_recompute { " and drop NORECOMPUTE so auto-update can keep it fresh" } else { "" }
            ),
            ddl: format!(
                "UPDATE STATISTICS {}.{} {} WITH FULLSCAN;{}",
                br(&st.schema_name), br(&st.table_name), br(&st.stats_name), norecompute_ddl
            ),
            impact_score: st.modification_counter as f64,
            metrics: vec![
                ("Modifications".into(), commas(st.modification_counter)),
                ("Histogram rows".into(), commas(st.rows)),
                ("Modified".into(), format!("{pct:.0} %")),
                ("Auto-update".into(), if st.no_recompute { "OFF (NORECOMPUTE)".into() } else { "on".into() }),
            ],
            confidence: "observed".into(),
        });
    }
    out
}

/// One rec per deprecated text/ntext/image column, with the ALTER COLUMN DDL.
pub fn deprecated_column_recs(bundle: &DmvBundle) -> Vec<Recommendation> {
    let rows = table_row_counts(&bundle.partition_stats);
    let mut out = Vec::new();
    for c in &bundle.deprecated_columns {
        let replacement = match c.type_name.to_ascii_lowercase().as_str() {
            "text" => "varchar(max)",
            "ntext" => "nvarchar(max)",
            "image" => "varbinary(max)",
            _ => continue,
        };
        let n = rows.get(&format!("{}.{}", c.schema_name, c.table_name).to_ascii_lowercase()).copied();
        out.push(Recommendation {
            kind: RecKind::AlterColumnType,
            priority: "low".into(),
            title: format!("Convert deprecated {} column {}.{}.{} to {}", c.type_name, c.schema_name, c.table_name, c.column_name, replacement),
            object: format!("{}.{}", c.schema_name, c.table_name),
            rationale: format!(
                "sys.columns declares {} as {} — deprecated since SQL Server 2005 and slated for removal. It cannot be used with most string functions, ORDER BY/DISTINCT, or in-row storage, and every access goes through the old text-pointer path. {} is the drop-in replacement with the same 2 GB limit.{}",
                c.column_name, c.type_name, replacement,
                n.map(|r| format!(" The table holds {} rows; the ALTER rewrites the LOB data, so schedule it.", commas(r))).unwrap_or_default()
            ),
            ddl: deprecated_column_ddl(c, replacement),
            impact_score: n.unwrap_or(0) as f64,
            metrics: vec![
                ("Column type".into(), c.type_name.clone()),
                ("Replacement".into(), replacement.into()),
            ],
            confidence: "observed".into(),
        });
    }
    out
}

/// Parameter-sniffing recs from Query Store: same query_id, ≥ 2 plans, and a
/// max-vs-average logical-read swing of ≥ 10×. Names the module when the
/// query belongs to one and shows the measured numbers.
pub fn sniffing_recs(bundle: &DmvBundle) -> Vec<Recommendation> {
    let mut out = Vec::new();
    for q in &bundle.query_skew {
        if q.plan_count < 2 || q.avg_logical_reads == 0 || q.executions < 50 { continue; }
        let ratio = q.max_logical_reads as f64 / q.avg_logical_reads as f64;
        if ratio < 10.0 { continue; }
        let subject = q.object_name.clone().unwrap_or_else(|| format!("query {}", q.query_id));
        let priority = if q.max_logical_reads >= 1_000_000 { "high" } else if q.max_logical_reads >= 100_000 { "medium" } else { "low" };
        let snippet: String = q.sql_text.chars().take(160).collect();
        out.push(Recommendation {
            kind: RecKind::ParameterSniffing,
            priority: priority.into(),
            title: format!("Parameter-sniffing skew in {subject}: {} plans, reads swing {:.0}× (avg {} → max {})", q.plan_count, ratio, commas(q.avg_logical_reads), commas(q.max_logical_reads)),
            object: subject.clone(),
            rationale: format!(
                "Query Store (query_id {}) holds {} distinct plans for this statement over {} executions; logical reads average {} but peak at {} — a {:.0}× swing, the signature of a plan compiled for a selective parameter value and reused for a non-selective one (or vice versa). Avg duration {:.0} ms, max {:.0} ms. Options, cheapest first: OPTION (RECOMPILE) on the statement (compile per call; fine under ~100 calls/s), OPTIMIZE FOR a representative value or UNKNOWN, a covering index that makes both shapes a seek, or Query Store plan forcing once you know which plan is right. Statement: {}",
                q.query_id, q.plan_count, commas(q.executions), commas(q.avg_logical_reads), commas(q.max_logical_reads), ratio,
                q.avg_duration_ms, q.max_duration_ms, snippet
            ),
            ddl: match &q.object_name {
                Some(m) => format!(
                    "-- Inside {m}: add to the skewed statement\n--   ... OPTION (RECOMPILE);\n-- or pin a typical value:\n--   ... OPTION (OPTIMIZE FOR (@param = <typical value>));\n-- or force the good plan from Query Store once identified:\n-- EXEC sp_query_store_force_plan @query_id = {}, @plan_id = <good plan_id>;",
                    q.query_id
                ),
                None => format!(
                    "-- Ad hoc statement (query_id {}): add OPTION (RECOMPILE) or OPTIMIZE FOR, or force the good plan:\n-- EXEC sp_query_store_force_plan @query_id = {}, @plan_id = <good plan_id>;",
                    q.query_id, q.query_id
                ),
            },
            impact_score: q.max_logical_reads as f64,
            metrics: vec![
                ("Plans".into(), format!("{}", q.plan_count)),
                ("Executions".into(), commas(q.executions)),
                ("Avg reads".into(), commas(q.avg_logical_reads)),
                ("Max reads".into(), commas(q.max_logical_reads)),
                ("Swing".into(), format!("{ratio:.0}×")),
            ],
            confidence: "observed".into(),
        });
    }
    out
}

/// Priority of a merge/drop-redundant rec by the storage the drop reclaims.
/// A redundant index on an EMPTY table is still redundant, but it is not worth
/// a slot in "what to fix first" above anything with measured cost: it goes to
/// `low` (informational downstream), and the rest rank by size within `medium`.
fn merge_priority(drop_reserved_kb: u64) -> &'static str {
    if drop_reserved_kb == 0 { "low" } else { "medium" }
}

/// The table's clustered index, if we can identify it. Prefers the explicit
/// `is_clustered` flag; for bundles that predate it, falls back to the PK on a
/// non-heap table (SQL Server clusters the PK by default). `None` for heaps and
/// for non-heap tables whose index metadata we never collected.
fn clustered_index_for<'a>(indexes: &'a [IndexMeta], schema: &str, table: &str, is_heap: bool) -> Option<&'a IndexMeta> {
    let on_table = |ix: &&IndexMeta| {
        ix.schema_name.eq_ignore_ascii_case(schema) && ix.table_name.eq_ignore_ascii_case(table)
    };
    if let Some(ix) = indexes.iter().filter(on_table).find(|ix| ix.is_clustered) {
        return Some(ix);
    }
    if is_heap { return None; }
    indexes.iter().filter(on_table).find(|ix| ix.is_primary_key)
}

/// Clustered-columnstore conversion DDL that actually compiles against the
/// table's current shape. SQL Server allows ONE clustered index per table, so:
///   heap                          → plain CREATE (DROP_EXISTING = OFF)
///   plain clustered index         → rebuild it in place with DROP_EXISTING = ON
///   constraint-backed clustered PK→ DROP CONSTRAINT, build the CCI, re-add the
///                                   PK as NONCLUSTERED (DROP_EXISTING fails
///                                   with Msg 1907 on a constraint index)
fn columnstore_ddl(schema: &str, table: &str, is_heap: bool, clustered: Option<&IndexMeta>) -> String {
    let head = "-- Validate workload is analytic/scan-heavy (not OLTP point lookups) first.\n-- Converting replaces the clustered rowstore; test in a non-prod copy.\n";
    match clustered {
        None if is_heap => format!(
            "{head}CREATE CLUSTERED COLUMNSTORE INDEX [CCI_{}] ON {}.{}\n  WITH (DROP_EXISTING = OFF, MAXDOP = 1);",
            idfrag(table), br(schema), br(table)
        ),
        None => format!(
            "{head}-- This table has a clustered index, but its name was not in the collected metadata.\n-- Replace <clustered_index_name> below (see sys.indexes WHERE index_id = 1); if it backs a\n-- PRIMARY KEY/UNIQUE constraint, DROP the constraint first and re-add it NONCLUSTERED after.\nCREATE CLUSTERED COLUMNSTORE INDEX [<clustered_index_name>] ON {}.{}\n  WITH (DROP_EXISTING = ON, MAXDOP = 1);",
            br(schema), br(table)
        ),
        Some(ix) if ix.is_primary_key || ix.is_unique => {
            let kind = if ix.is_primary_key { "PRIMARY KEY" } else { "UNIQUE" };
            let keys = if ix.key_columns.is_empty() {
                "<key columns>".to_string()
            } else {
                ix.key_columns.iter().map(|c| br(c)).collect::<Vec<_>>().join(", ")
            };
            format!(
                "{head}-- {} is a {kind} constraint, so DROP_EXISTING cannot replace it (Msg 1907).\n-- Any FOREIGN KEY referencing this {kind} must be dropped before, and re-created after.\nALTER TABLE {}.{} DROP CONSTRAINT {};\nCREATE CLUSTERED COLUMNSTORE INDEX [CCI_{}] ON {}.{}\n  WITH (MAXDOP = 1);\nALTER TABLE {}.{} ADD CONSTRAINT {} {kind} NONCLUSTERED ({});",
                ix.index_name,
                br(schema), br(table), br(&ix.index_name),
                idfrag(table), br(schema), br(table),
                br(schema), br(table), br(&ix.index_name), keys
            )
        }
        Some(ix) => format!(
            "{head}-- Rebuilds the existing clustered index {} in place as columnstore.\nCREATE CLUSTERED COLUMNSTORE INDEX {} ON {}.{}\n  WITH (DROP_EXISTING = ON, MAXDOP = 1);",
            ix.index_name, br(&ix.index_name), br(schema), br(table)
        ),
    }
}

// ===========================================================================
// Index-advisor cross-group de-dup: one index per target, not one per group.
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
    consolidate_missing_indexes_traced(missing).into_iter().map(|(m, _)| m).collect()
}

/// [`consolidate_missing_indexes`] that also returns, per survivor, the seek
/// counts of every DMV group folded into it — so a rec can say "2 groups
/// merged; seeks summed: 1,485 + 1,485" instead of presenting 2,970 as one
/// measurement.
pub fn consolidate_missing_indexes_traced(missing: &[MissingIndex]) -> Vec<(MissingIndex, Vec<u64>)> {
    // The DMV builds the index key as equality columns first, then inequality
    // columns. Compare on that full ordered key.
    fn full_key(m: &MissingIndex) -> Vec<String> {
        m.equality_columns.iter().chain(m.inequality_columns.iter()).cloned().collect()
    }

    // Group by (schema, table), case-insensitively, preserving input order.
    let mut groups: BTreeMap<(String, String), Vec<(MissingIndex, Vec<u64>)>> = BTreeMap::new();
    for m in missing {
        if full_key(m).is_empty() { continue; }
        groups
            .entry((m.schema_name.to_lowercase(), m.table_name.to_lowercase()))
            .or_default()
            .push((m.clone(), vec![m.user_seeks]));
    }

    let mut out: Vec<(MissingIndex, Vec<u64>)> = Vec::new();
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
                    let ki = full_key(&list[i].0);
                    let kj = full_key(&list[j].0);
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
                        let (absorbed, parts) = list.remove(j);
                        // Index i may have shifted if j < i.
                        let i2 = if j < i { i - 1 } else { i };
                        union_includes(&mut list[i2].0.included_columns, &absorbed.included_columns);
                        // Aggregate evidence onto the survivor.
                        list[i2].0.avg_user_impact = list[i2].0.avg_user_impact.max(absorbed.avg_user_impact);
                        list[i2].0.avg_total_user_cost = list[i2].0.avg_total_user_cost.max(absorbed.avg_total_user_cost);
                        list[i2].0.user_seeks = list[i2].0.user_seeks.saturating_add(absorbed.user_seeks);
                        list[i2].1.extend(parts);
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
        if reads != 0 || u.user_updates <= min_updates || u.is_heap() { continue; }
        let key = (u.schema_name.to_lowercase(), u.table_name.to_lowercase(), u.index_name.to_lowercase());
        if let Some(ix) = meta.get(&key) {
            if ix.is_primary_key || ix.is_unique || ix.is_clustered { continue; }
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
            avg_total_user_cost: cost, ..Default::default()
        }
    }

    fn ix(table: &str, name: &str, keys: &[&str], unique: bool, pk: bool) -> IndexMeta {
        IndexMeta {
            schema_name: "dbo".into(),
            table_name: table.into(),
            index_name: name.into(),
            is_unique: unique,
            is_primary_key: pk,
            is_clustered: false,
            key_columns: keys.iter().map(|s| s.to_string()).collect(),
            included_columns: vec![], ..Default::default()
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
            no_stats_row: false, ..Default::default()
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

    // ---- (f) cost weighting: findings carry the object + its measured size --

    fn ps(schema: &str, table: &str, index: Option<&str>, rows: u64, reserved_kb: u64) -> PartitionStats {
        PartitionStats {
            schema_name: schema.to_string(),
            table_name: table.to_string(),
            index_name: index.map(|s| s.to_string()),
            row_count: rows,
            reserved_kb,
            used_kb: reserved_kb,
            data_kb: reserved_kb, ..Default::default()
        }
    }

    #[test]
    fn structural_findings_carry_object_with_measured_size() {
        // Two heaps of wildly different size. Severity alone cannot tell them
        // apart below the 1M threshold — the ObjectRef is what makes them
        // rankable, so it must carry the real row count and reserved space.
        let bundle = DmvBundle {
            partition_stats: vec![
                ps("dbo", "tiny_heap", None, 1_200, 64),
                ps("dbo", "fact_events", None, 262_000_000, 41_000_000),
            ],
            ..Default::default()
        };
        let findings = analyze(&bundle).findings;
        let heaps: Vec<_> = findings.iter().filter(|f| f.rule.0 == "structure.heap_table").collect();
        assert_eq!(heaps.len(), 2, "{findings:#?}");

        let big = heaps.iter().find(|f| f.object.as_ref().unwrap().table == "fact_events").unwrap();
        let o = big.object.as_ref().unwrap();
        assert_eq!(o.schema, "dbo");
        assert_eq!(o.row_count, Some(262_000_000));
        assert_eq!(o.reserved_kb, Some(41_000_000));
        assert_eq!(o.key(), "dbo.fact_events");

        let small = heaps.iter().find(|f| f.object.as_ref().unwrap().table == "tiny_heap").unwrap();
        assert_eq!(small.object.as_ref().unwrap().row_count, Some(1_200));
    }

    #[test]
    fn reserved_kb_sums_across_indexes_rows_take_the_max() {
        // Each index of a table reports the SAME row set, so rows must be MAX
        // (not a 3x-inflated sum) while space must be SUM (each index really
        // does occupy its own pages).
        let bundle = DmvBundle {
            partition_stats: vec![
                ps("dbo", "orders", None, 500_000, 1_000),
                ps("dbo", "orders", Some("IX_a"), 500_000, 400),
                ps("dbo", "orders", Some("IX_b"), 500_000, 600),
            ],
            ..Default::default()
        };
        let f = analyze(&bundle)
            .findings
            .into_iter()
            .find(|f| f.rule.0 == "structure.heap_table")
            .expect("heap finding");
        let o = f.object.unwrap();
        assert_eq!(o.row_count, Some(500_000), "rows must not be summed across indexes");
        assert_eq!(o.reserved_kb, Some(2_000), "reserved space must be summed");
    }

    #[test]
    fn object_size_is_none_when_the_scan_never_measured_it() {
        // A missing-index DMV row for a table with no partition-stats entry.
        // Unknown size must stay None — reporting 0 rows would rank a real
        // table as if it were empty.
        let bundle = DmvBundle {
            missing_indexes: vec![mi("Unscanned", &["CustomerId"], &[], &[], 80.0, 40, 3.0)],
            ..Default::default()
        };
        let f = analyze(&bundle)
            .findings
            .into_iter()
            .find(|f| f.rule.0 == "dmv.missing_index")
            .expect("missing-index finding");
        let o = f.object.expect("table identity is known even when size is not");
        assert_eq!(o.table, "Unscanned");
        assert_eq!(o.row_count, None);
        assert_eq!(o.reserved_kb, None);
    }

    // ---- columnstore DDL vs the table's clustered index ----------------------

    fn big_scanned(table: &str, index: Option<&str>, scans: u64) -> DmvBundle {
        DmvBundle {
            index_usage: vec![usage(table, index.unwrap_or("IX_x"), 0, scans, 0, 0)],
            partition_stats: vec![PartitionStats {
                schema_name: "dbo".into(),
                table_name: table.into(),
                index_name: index.map(|s| s.to_string()),
                row_count: 5_000_000,
                reserved_kb: 2_000_000,
                used_kb: 2_000_000,
                data_kb: 1_900_000, ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn cci(recs: &[Recommendation]) -> Option<&Recommendation> {
        recs.iter().find(|r| r.kind == RecKind::ColumnstoreCandidate)
    }

    #[test]
    fn columnstore_on_clustered_pk_drops_constraint_and_readds_nonclustered() {
        let mut b = big_scanned("Claims", Some("PK_Claims"), 5_000);
        let mut pk = ix("Claims", "PK_Claims", &["ClaimId"], true, true);
        pk.is_clustered = true;
        b.indexes.push(pk);
        let recs = advise(&b);
        let r = cci(&recs).expect("columnstore rec");
        assert!(r.ddl.contains("DROP CONSTRAINT [PK_Claims]"), "{}", r.ddl);
        assert!(r.ddl.contains("PRIMARY KEY NONCLUSTERED ([ClaimId])"), "{}", r.ddl);
        assert!(r.ddl.contains("FOREIGN KEY"), "must warn about referencing FKs: {}", r.ddl);
        assert!(!r.ddl.contains("DROP_EXISTING = OFF"), "{}", r.ddl);
        assert!(r.rationale.contains("reset on restart"), "{}", r.rationale);
    }

    #[test]
    fn columnstore_on_pk_without_clustered_flag_still_assumes_pk_is_clustered() {
        // Older bundles have no is_clustered; a non-heap table's PK is clustered by default.
        let mut b = big_scanned("Claims", Some("PK_Claims"), 5_000);
        b.indexes.push(ix("Claims", "PK_Claims", &["ClaimId"], true, true));
        let r = cci(&advise(&b)).expect("columnstore rec").ddl.clone();
        assert!(r.contains("DROP CONSTRAINT [PK_Claims]"), "{r}");
        assert!(!r.contains("DROP_EXISTING = OFF"), "{r}");
    }

    #[test]
    fn columnstore_on_plain_clustered_index_uses_drop_existing_on() {
        let mut b = big_scanned("Facts", Some("CX_Facts"), 5_000);
        let mut cx = ix("Facts", "CX_Facts", &["LoadDate"], false, false);
        cx.is_clustered = true;
        b.indexes.push(cx);
        let r = cci(&advise(&b)).expect("columnstore rec").ddl.clone();
        assert!(r.contains("CREATE CLUSTERED COLUMNSTORE INDEX [CX_Facts]"), "{r}");
        assert!(r.contains("DROP_EXISTING = ON"), "{r}");
        assert!(!r.contains("DROP CONSTRAINT"), "{r}");
    }

    #[test]
    fn columnstore_on_heap_keeps_plain_create() {
        let b = big_scanned("Facts", None, 5_000);
        let r = cci(&advise(&b)).expect("columnstore rec").ddl.clone();
        assert!(r.contains("[CCI_Facts]") && r.contains("DROP_EXISTING = OFF"), "{r}");
    }

    #[test]
    fn columnstore_needs_a_real_scan_sample() {
        // 18 scans minutes after a restart is a DBA's own test runs, not a workload.
        let b = big_scanned("Facts", None, 18);
        assert!(cci(&advise(&b)).is_none(), "a handful of scans must not rank a columnstore rec");
    }

    // ---- merge-index Reads chip reflects measured usage -----------------------

    fn reads_chip(r: &Recommendation) -> Option<&str> {
        r.metrics.iter().find(|(l, _)| l == "Reads").map(|(_, v)| v.as_str())
    }

    #[test]
    fn prefix_redundant_index_with_seeks_reports_real_reads() {
        let b = DmvBundle {
            indexes: vec![
                ix("Orders", "IX_cust", &["CustomerId"], false, false),
                ix("Orders", "IX_cust_date", &["CustomerId", "OrderDate"], false, false),
            ],
            index_usage: vec![usage("Orders", "ix_CUST", 1_234, 5, 0, 77)],
            ..Default::default()
        };
        let recs = advise(&b);
        let r = recs.iter().find(|r| r.kind == RecKind::MergeIndex && r.object.ends_with("IX_cust")).expect("merge rec");
        assert_eq!(reads_chip(r), Some("1,239"));
        assert!(r.metrics.iter().any(|(l, v)| l == "Writes" && v == "77"));
        assert_eq!(r.confidence, "observed");
        assert!(!r.metrics.iter().any(|(_, v)| v.contains("unique")));
    }

    #[test]
    fn duplicate_index_with_no_usage_row_is_estimated_not_zero() {
        let b = DmvBundle {
            indexes: vec![
                ix("Orders", "IX_a", &["CustomerId"], false, false),
                ix("Orders", "IX_b", &["CustomerId"], false, false),
            ],
            ..Default::default()
        };
        let recs = advise(&b);
        let r = recs.iter().find(|r| r.kind == RecKind::MergeIndex).expect("merge rec");
        assert_eq!(reads_chip(r), Some("no usage recorded since restart"));
        assert_eq!(r.confidence, "estimated");
    }

    #[test]
    fn zero_filled_usage_row_from_left_join_is_not_observed_zero() {
        // The backend collector LEFT JOINs sys.dm_db_index_usage_stats and
        // zero-fills, flagging `no_stats_row`. That must read as "not observed",
        // never as a measured "0".
        let mut u = usage("Orders", "IX_a", 0, 0, 0, 0);
        u.no_stats_row = true;
        let b = DmvBundle {
            indexes: vec![
                ix("Orders", "IX_a", &["CustomerId"], false, false),
                ix("Orders", "IX_b", &["CustomerId", "OrderDate"], false, false),
            ],
            index_usage: vec![u],
            ..Default::default()
        };
        let recs = advise(&b);
        let r = recs.iter().find(|r| r.kind == RecKind::MergeIndex && r.object.ends_with("IX_a")).expect("merge rec");
        assert_eq!(reads_chip(r), Some("no usage recorded since restart"));
        assert_eq!(r.confidence, "estimated");
    }

    // ---- counter age (D2) + missing-index history (D20) + size-ranked merges (D16)

    fn chip<'a>(r: &'a Recommendation, label: &str) -> Option<&'a str> {
        r.metrics.iter().find(|(l, _)| l == label).map(|(_, v)| v.as_str())
    }

    fn dropped_index_bundle() -> DmvBundle {
        DmvBundle {
            indexes: vec![ix("Orders", "IX_dead", &["Note"], false, false)],
            index_usage: vec![usage("Orders", "IX_dead", 0, 0, 0, 150_000)],
            missing_indexes: vec![mi("Orders", &["CustomerId"], &[], &["Total"], 90.0, 8, 130.0)],
            ..Default::default()
        }
    }

    #[test]
    fn young_counters_downgrade_observed_to_estimated_and_say_how_young() {
        // The trap: '0 reads' measured over ten minutes presented as observed.
        let mut b = dropped_index_bundle();
        b.counter_age_secs = Some(600);
        b.counters_since = Some("2026-08-22T19:18:50Z".into());
        let recs = advise(&b);
        let drop = recs.iter().find(|r| r.kind == RecKind::DropIndex).expect("drop rec");
        assert_eq!(drop.confidence, "estimated");
        assert!(drop.rationale.contains("counters cover 0.2 h since restart (2026-08-22T19:18:50Z)"), "{}", drop.rationale);
        assert!(drop.rationale.contains("sample, not a verdict"));
        assert_eq!(chip(drop, "Counters since restart"), Some("0.2 h"));
        // Every kind carries the stamp, including SQL Server's own projections.
        let create = recs.iter().find(|r| r.kind == RecKind::CreateIndex).expect("create rec");
        assert!(create.rationale.contains("counters cover 0.2 h"));
        assert_eq!(create.confidence, "estimated");
    }

    #[test]
    fn mature_counters_keep_observed_confidence_but_still_state_the_window() {
        let mut b = dropped_index_bundle();
        b.counter_age_secs = Some(86 * 86_400);
        let recs = advise(&b);
        let drop = recs.iter().find(|r| r.kind == RecKind::DropIndex).unwrap();
        assert_eq!(drop.confidence, "observed");
        assert!(drop.rationale.contains("counters cover 2,064 h (86 d) since restart"), "{}", drop.rationale);
        assert!(!drop.rationale.contains("sample, not a verdict"));
    }

    #[test]
    fn unknown_counter_age_adds_no_claim() {
        let recs = advise(&dropped_index_bundle());
        let drop = recs.iter().find(|r| r.kind == RecKind::DropIndex).unwrap();
        assert_eq!(drop.confidence, "observed");
        assert!(!drop.rationale.contains("counters cover"));
        assert!(chip(drop, "Counters since restart").is_none());
    }

    #[test]
    fn counter_age_never_touches_heuristic_confidence() {
        let mut recs = vec![Recommendation {
            kind: RecKind::ColumnstoreCandidate,
            priority: "high".into(),
            title: String::new(),
            object: String::new(),
            rationale: String::new(),
            ddl: String::new(),
            impact_score: 0.0,
            metrics: vec![],
            confidence: "heuristic".into(),
        }];
        apply_counter_age(&mut recs, Some(60), None);
        assert_eq!(recs[0].confidence, "heuristic");
        assert!(recs[0].rationale.contains("counters cover 0.0 h since restart;"));
    }

    #[test]
    fn counter_age_phrase_bands() {
        assert_eq!(counter_age_phrase(179), "counters cover 0.0 h since restart");
        assert_eq!(counter_age_phrase(7 * 3600), "counters cover 7.0 h since restart");
        assert_eq!(counter_age_phrase(30 * 3600), "counters cover 30 h since restart");
        assert_eq!(counter_age_phrase(3 * 86_400), "counters cover 72 h (3 d) since restart");
    }

    #[test]
    fn create_index_reads_back_persistence_from_the_monitor() {
        let mut b = dropped_index_bundle();
        b.missing_index_history = vec![MissingIndexHistory {
            schema_name: "DBO".into(),
            table_name: "orders".into(),
            days_seen: 6,
            days_observed: 7,
        }];
        let recs = advise(&b);
        let create = recs.iter().find(|r| r.kind == RecKind::CreateIndex).unwrap();
        assert!(create.rationale.contains("on 6 of the last 7 monitored day(s)"), "{}", create.rationale);
        assert_eq!(chip(create, "Seen (days)"), Some("6 of 7"));
        // No history → no claim either way.
        let plain = advise(&dropped_index_bundle());
        let create = plain.iter().find(|r| r.kind == RecKind::CreateIndex).unwrap();
        assert!(!create.rationale.contains("monitored day"));
        assert!(chip(create, "Seen (days)").is_none());
    }

    #[test]
    fn redundant_index_on_an_empty_table_is_low_priority_and_ranks_last() {
        let ps = |table: &str, index: &str, kb: u64| PartitionStats {
            schema_name: "dbo".into(),
            table_name: table.into(),
            index_name: Some(index.into()),
            row_count: if kb == 0 { 0 } else { 1000 },
            reserved_kb: kb,
            used_kb: kb,
            data_kb: kb, ..Default::default()
        };
        let b = DmvBundle {
            indexes: vec![
                ix("Empty", "IX_e", &["tenant_id"], false, false),
                ix("Empty", "IX_e_date", &["tenant_id", "d"], false, false),
                ix("Big", "IX_b", &["tenant_id"], false, false),
                ix("Big", "IX_b_date", &["tenant_id", "d"], false, false),
                ix("Mid", "IX_m", &["tenant_id"], false, false),
                ix("Mid", "IX_m_date", &["tenant_id", "d"], false, false),
            ],
            partition_stats: vec![ps("Empty", "IX_e", 0), ps("Big", "IX_b", 21_300), ps("Mid", "IX_m", 2_150)],
            ..Default::default()
        };
        let recs: Vec<_> = advise(&b).into_iter().filter(|r| r.kind == RecKind::MergeIndex).collect();
        let names: Vec<&str> = recs.iter().map(|r| r.object.rsplit('.').next().unwrap()).collect();
        assert_eq!(names, vec!["IX_b", "IX_m", "IX_e"], "{names:?}");
        assert_eq!(recs[0].priority, "medium");
        assert_eq!(recs[2].priority, "low");
        assert!(recs[0].impact_score > recs[1].impact_score && recs[1].impact_score > recs[2].impact_score);
    }
}
