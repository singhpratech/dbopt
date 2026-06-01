use crate::findings::{Finding, Severity, RuleId, Location};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("xml: {0}")]
    Xml(String),
}

/// A `<MissingIndex>` recommendation the optimizer emitted into the plan. The
/// equality/inequality/included column split mirrors how a covering index is
/// constructed: equality columns first, inequality next, the rest INCLUDEd.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MissingIndexInfo {
    pub impact: f64,
    pub database: String,
    pub schema: String,
    pub table: String,
    pub equality: Vec<String>,
    pub inequality: Vec<String>,
    pub included: Vec<String>,
}

/// A flagged column under `<ColumnsWithNoStatistics>` (table + column).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColumnRef {
    pub table: String,
    pub column: String,
}

/// Detail captured from a `<PlanAffectingConvert>` warning so we can name the
/// exact expression the optimizer had to implicitly convert.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConvertInfo {
    pub issue: String,
    pub expression: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanNode {
    pub physical_op: String,
    pub logical_op: String,
    pub estimated_total_subtree_cost: f64,
    pub estimated_rows: f64,
    pub estimated_io: f64,
    pub estimated_cpu: f64,
    pub node_id: i32,
    pub children: Vec<PlanNode>,
    pub warnings: Vec<String>,

    // --- enriched (offline-deep) detail; all default-empty so existing
    // consumers (report::plan_treemap, lib::derive_findings) are unaffected ---
    /// Estimated executions of this operator (`EstimateRebinds + EstimateRewinds
    /// + 1`-ish); SQL Server exposes it directly as `EstimatedExecutionMode`/
    /// counters. We use the showplan `EstimateRebinds`/`EstimateRewinds` sum as a
    /// proxy for "how many times the inner side runs" (lookups, spools).
    pub estimated_executions: f64,
    /// Actual rows from `<RunTimeInformation>` (actual plans only). `None` for an
    /// estimated-only plan, in which case the est-vs-actual skew rule stays quiet.
    pub actual_rows: Option<f64>,
    /// Number of executions reported at runtime (sum across threads).
    pub actual_executions: Option<f64>,
    /// True when this operator carries a residual `<Predicate>` (a filter applied
    /// *after* the access, i.e. rows read then discarded). Seek predicates do not
    /// set this — only the leftover `Predicate` element does.
    pub has_residual_predicate: bool,
    /// True when this operator has `<SeekPredicates>` (a genuine seek). Used to
    /// avoid mislabeling a seek-with-residual as a pure scan.
    pub has_seek_predicate: bool,
    /// Missing-index recommendations attached to this operator's subtree.
    pub missing_indexes: Vec<MissingIndexInfo>,
    /// Columns the optimizer found had no statistics (from the warning child).
    pub no_stats_columns: Vec<ColumnRef>,
    /// Detail of a plan-affecting implicit conversion, when present.
    pub convert: Option<ConvertInfo>,

    // --- Lookup-operator detail (Key/RID Lookup). All default-empty so existing
    // consumers are unaffected. Captured so plan.lookup can emit CONCRETE
    // covering-index DDL instead of placeholder INCLUDE columns. ---
    /// `<Object>` the lookup reads from: the base-table identifier and the index
    /// the seek used (for a Key Lookup this is the clustered index / PK that the
    /// lookup probes). Used to name the table the covering index belongs on.
    pub object_schema: String,
    pub object_table: String,
    pub object_index: String,
    /// Output columns the lookup fetches — the `<DefinedValues>/<DefinedValue>/
    /// <ColumnReference>` (or `<OutputList>`) children of THIS operator. These are
    /// exactly the columns missing from the nonclustered index, i.e. the real
    /// INCLUDE list for a covering index.
    pub output_columns: Vec<ColumnRef>,
    /// Seek key columns from the lookup's `<SeekPredicates>` (the columns the
    /// lookup joins back on — typically the clustering key). Captured so the
    /// emitted DDL and width heuristic have the full key picture.
    pub seek_columns: Vec<ColumnRef>,
}

/// Known SQL Server plan warning elements (children of `<Warnings>`), plus the
/// catch-all `*Warning` suffix. The `<Warnings>` container itself is excluded.
fn is_warning_elem(name: &str) -> bool {
    matches!(
        name,
        "NoJoinPredicate"
            | "ColumnsWithNoStatistics"
            | "PlanAffectingConvert"
            | "SpillToTempDb"
            | "MemoryGrantWarning"
    ) || name.ends_with("Warning")
}

/// Strip the `[...]` bracket-quoting SQL Server uses for identifiers inside
/// showplan attribute values so recommendations read like hand-written DDL.
fn unbracket(s: &str) -> String {
    s.trim().trim_start_matches('[').trim_end_matches(']').to_string()
}

/// Re-bracket an identifier for safe emission into generated DDL.
fn bracket(s: &str) -> String {
    let inner = unbracket(s);
    if inner.is_empty() { inner } else { format!("[{}]", inner) }
}

/// Read an attribute value off a start/empty element by key (UTF-8, unescaped).
fn attr_val(e: &quick_xml::events::BytesStart, key: &str) -> Option<String> {
    for attr in e.attributes().with_checks(false).flatten() {
        if std::str::from_utf8(attr.key.as_ref()).unwrap_or("") == key {
            return Some(attr.unescape_value().unwrap_or_default().to_string());
        }
    }
    None
}

/// Populate the core estimate attributes shared by Start and Empty `RelOp`s.
fn fill_relop_attrs(node: &mut PlanNode, e: &quick_xml::events::BytesStart) {
    let mut rebinds = 0.0f64;
    let mut rewinds = 0.0f64;
    for attr in e.attributes().with_checks(false).flatten() {
        let key = std::str::from_utf8(attr.key.as_ref()).unwrap_or("");
        let val = attr.unescape_value().unwrap_or_default().to_string();
        match key {
            "PhysicalOp" => node.physical_op = val,
            "LogicalOp" => node.logical_op = val,
            "EstimatedTotalSubtreeCost" => node.estimated_total_subtree_cost = val.parse().unwrap_or(0.0),
            "EstimateRows" | "EstimatedRowsRead" => node.estimated_rows = val.parse().unwrap_or(0.0),
            "EstimateIO" => node.estimated_io = val.parse().unwrap_or(0.0),
            "EstimateCPU" => node.estimated_cpu = val.parse().unwrap_or(0.0),
            "NodeId" => node.node_id = val.parse().unwrap_or(0),
            "EstimateRebinds" => rebinds = val.parse().unwrap_or(0.0),
            "EstimateRewinds" => rewinds = val.parse().unwrap_or(0.0),
            _ => {}
        }
    }
    // Executions of the operator's inner side ~ rebinds + rewinds + 1.
    node.estimated_executions = rebinds + rewinds + 1.0;
}

pub fn parse(xml: &str) -> Result<PlanNode, PlanError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut stack: Vec<PlanNode> = Vec::new();
    let mut root: Option<PlanNode> = None;
    let mut buf = Vec::new();

    // Parsing context for the multi-level MissingIndex / warning subtrees.
    // `<MissingIndexes>` is a child of `<QueryPlan>` (not of a RelOp), so it can
    // appear before any RelOp opens. Collect them top-level and attach to the
    // root node once parsing finishes.
    let mut pending_missing: Vec<MissingIndexInfo> = Vec::new();
    let mut cur_missing: Option<MissingIndexInfo> = None;
    let mut cur_group_impact: f64 = 0.0;
    let mut cur_usage: Option<String> = None; // EQUALITY / INEQUALITY / INCLUDE
    let mut in_no_stats = false;
    // Depth of element nesting since the current RelOp opened — used to scope a
    // `Predicate` element to *this* operator (depth 1 child) rather than a
    // descendant operator's predicate.
    let mut depth_since_relop: Vec<i32> = Vec::new();

    // --- Lookup-operator capture context ---
    // We capture <Object>/<DefinedValues>/<SeekPredicates> column references and
    // scope them to the innermost RelOp ONLY while that RelOp is a Key/RID Lookup
    // (and the cursor is inside the relevant child element). This keeps unrelated
    // ColumnReference elements (scalar trees, joins, other operators) out.
    let mut in_defined_values = false;
    let mut in_seek_predicates = false;
    // True iff the innermost open RelOp on the stack is a Key/RID Lookup.
    let top_is_lookup = |stack: &Vec<PlanNode>| -> bool {
        stack.last().map(|n| {
            n.physical_op.eq_ignore_ascii_case("Key Lookup")
                || n.physical_op.eq_ignore_ascii_case("RID Lookup")
        }).unwrap_or(false)
    };
    // Operators whose <Object>/seek columns we capture so a sibling lookup can
    // identify the nonclustered seek that feeds it: lookups themselves AND the
    // Index Seek / Scan operators that may be its partner.
    let top_capture_object = |stack: &Vec<PlanNode>| -> bool {
        stack.last().map(|n| {
            let op = n.physical_op.to_ascii_lowercase();
            op.contains("lookup") || op.contains("index seek") || op.contains("index scan")
        }).unwrap_or(false)
    };

    loop {
        match reader.read_event_into(&mut buf).map_err(|e| PlanError::Xml(e.to_string()))? {
            Event::Eof => break,
            Event::Start(e) => {
                let raw = e.name();
                let name = std::str::from_utf8(raw.as_ref()).unwrap_or("").to_string();
                if name == "RelOp" {
                    let mut node = PlanNode::default();
                    fill_relop_attrs(&mut node, &e);
                    stack.push(node);
                    depth_since_relop.push(0);
                } else {
                    if let Some(d) = depth_since_relop.last_mut() { *d += 1; }
                    match name.as_str() {
                        "MissingIndexGroup" => {
                            cur_group_impact = attr_val(&e, "Impact")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0.0);
                        }
                        "MissingIndex" => {
                            cur_missing = Some(MissingIndexInfo {
                                impact: cur_group_impact,
                                database: unbracket(&attr_val(&e, "Database").unwrap_or_default()),
                                schema: unbracket(&attr_val(&e, "Schema").unwrap_or_default()),
                                table: unbracket(&attr_val(&e, "Table").unwrap_or_default()),
                                ..Default::default()
                            });
                        }
                        "ColumnGroup" => {
                            cur_usage = attr_val(&e, "Usage");
                        }
                        "ColumnsWithNoStatistics" => {
                            in_no_stats = true;
                            if let Some(top) = stack.last_mut() { top.warnings.push(name.clone()); }
                        }
                        "Predicate" => {
                            // A residual `Predicate` for THIS operator sits just
                            // under the access element: RelOp > <Scan> > Predicate
                            // (depth 2), or directly RelOp > Predicate (depth 1).
                            // It is scoped to the innermost RelOp frame, so a child
                            // operator's predicate never bleeds up. Deeper Predicate
                            // elements (inside ScalarOperator trees) are ignored.
                            if let (Some(&d), Some(top)) = (depth_since_relop.last(), stack.last_mut()) {
                                if d == 1 || d == 2 { top.has_residual_predicate = true; }
                            }
                        }
                        "SeekPredicates" => {
                            if let (Some(&d), Some(top)) = (depth_since_relop.last(), stack.last_mut()) {
                                if d == 1 || d == 2 { top.has_seek_predicate = true; }
                            }
                            if top_capture_object(&stack) { in_seek_predicates = true; }
                        }
                        "DefinedValues" => {
                            if top_is_lookup(&stack) { in_defined_values = true; }
                        }
                        "Object" => {
                            // The <Object> a seek/lookup reads from: base table + the
                            // index used. Captured for lookups AND seeks/scans so a
                            // lookup can find the sibling nonclustered seek that feeds
                            // it. First <Object> per operator wins (the access target).
                            if top_capture_object(&stack) {
                                if let Some(top) = stack.last_mut() {
                                    if top.object_table.is_empty() {
                                        top.object_schema = unbracket(&attr_val(&e, "Schema").unwrap_or_default());
                                        top.object_table = unbracket(&attr_val(&e, "Table").unwrap_or_default());
                                        top.object_index = unbracket(&attr_val(&e, "Index").unwrap_or_default());
                                    }
                                }
                            }
                        }
                        "PlanAffectingConvert" => {
                            if let Some(top) = stack.last_mut() {
                                top.warnings.push(name.clone());
                                top.convert = Some(ConvertInfo {
                                    issue: attr_val(&e, "ConvertIssue").unwrap_or_default(),
                                    expression: attr_val(&e, "Expression").unwrap_or_default(),
                                });
                            }
                        }
                        other if is_warning_elem(other) => {
                            if let Some(top) = stack.last_mut() { top.warnings.push(name.clone()); }
                        }
                        _ => {}
                    }
                }
            }
            Event::Empty(e) => {
                let raw = e.name();
                let name = std::str::from_utf8(raw.as_ref()).unwrap_or("").to_string();
                if name == "RelOp" {
                    let mut node = PlanNode::default();
                    fill_relop_attrs(&mut node, &e);
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    } else {
                        root = Some(node);
                    }
                } else {
                    // Empty elements do not change nesting depth, so we read the
                    // *current* depth (the open-tag count since the RelOp) to scope
                    // them. An empty <SeekPredicates/> sits at depth 1 (RelOp>X)
                    // because its parent <IndexScan> already incremented depth.
                    let cur_depth = depth_since_relop.last().copied().unwrap_or(0);
                    match name.as_str() {
                        "SeekPredicates" => {
                            if cur_depth == 1 || cur_depth == 2 {
                                if let Some(top) = stack.last_mut() { top.has_seek_predicate = true; }
                            }
                        }
                        "Object" => {
                            if top_capture_object(&stack) {
                                if let Some(top) = stack.last_mut() {
                                    if top.object_table.is_empty() {
                                        top.object_schema = unbracket(&attr_val(&e, "Schema").unwrap_or_default());
                                        top.object_table = unbracket(&attr_val(&e, "Table").unwrap_or_default());
                                        top.object_index = unbracket(&attr_val(&e, "Index").unwrap_or_default());
                                    }
                                }
                            }
                        }
                        "Predicate" => {
                            if cur_depth == 1 || cur_depth == 2 {
                                if let Some(top) = stack.last_mut() { top.has_residual_predicate = true; }
                            }
                        }
                        "Column" | "ColumnReference" => {
                            let col = unbracket(&attr_val(&e, "Column")
                                .or_else(|| attr_val(&e, "Name"))
                                .unwrap_or_default());
                            // Lookup output columns (DefinedValues, lookup only) and
                            // seek key columns (SeekPredicates, lookups + seeks) feed
                            // the covering DDL. Scoped to the relevant child element.
                            if !col.is_empty() {
                                let table = unbracket(&attr_val(&e, "Table").unwrap_or_default());
                                if in_defined_values && top_is_lookup(&stack) {
                                    if let Some(top) = stack.last_mut() {
                                        let cr = ColumnRef { table, column: col.clone() };
                                        if !top.output_columns.iter().any(|c| c.column == cr.column) {
                                            top.output_columns.push(cr);
                                        }
                                    }
                                } else if in_seek_predicates && top_capture_object(&stack) {
                                    if let Some(top) = stack.last_mut() {
                                        // Only the indexed object's own columns are
                                        // seek KEYS; a ColumnReference naming a
                                        // different table (or none) is the comparand
                                        // (value side) and must not be treated as a
                                        // key column of this index.
                                        let same_table = top.object_table.is_empty()
                                            || table.is_empty()
                                            || table.eq_ignore_ascii_case(&top.object_table);
                                        let cr = ColumnRef { table, column: col.clone() };
                                        if same_table && !top.seek_columns.iter().any(|c| c.column == cr.column) {
                                            top.seek_columns.push(cr);
                                        }
                                    }
                                }
                            }
                            if in_no_stats {
                                let table = unbracket(&attr_val(&e, "Table").unwrap_or_default());
                                if !col.is_empty() {
                                    if let Some(top) = stack.last_mut() {
                                        top.no_stats_columns.push(ColumnRef { table, column: col });
                                    }
                                }
                            } else if let (Some(mi), Some(usage)) = (cur_missing.as_mut(), cur_usage.as_ref()) {
                                if !col.is_empty() {
                                    match usage.as_str() {
                                        "EQUALITY" => mi.equality.push(col),
                                        "INEQUALITY" => mi.inequality.push(col),
                                        "INCLUDE" => mi.included.push(col),
                                        _ => {}
                                    }
                                }
                            }
                        }
                        "RunTimeCountersPerThread" => {
                            if let Some(top) = stack.last_mut() {
                                if let Some(v) = attr_val(&e, "ActualRows").and_then(|v| v.parse::<f64>().ok()) {
                                    top.actual_rows = Some(top.actual_rows.unwrap_or(0.0) + v);
                                }
                                if let Some(v) = attr_val(&e, "ActualExecutions").and_then(|v| v.parse::<f64>().ok()) {
                                    top.actual_executions = Some(top.actual_executions.unwrap_or(0.0) + v);
                                }
                            }
                        }
                        "PlanAffectingConvert" => {
                            if let Some(top) = stack.last_mut() {
                                top.warnings.push(name.clone());
                                top.convert = Some(ConvertInfo {
                                    issue: attr_val(&e, "ConvertIssue").unwrap_or_default(),
                                    expression: attr_val(&e, "Expression").unwrap_or_default(),
                                });
                            }
                        }
                        "ColumnsWithNoStatistics" => {
                            if let Some(top) = stack.last_mut() { top.warnings.push(name.clone()); }
                        }
                        other if is_warning_elem(other) => {
                            if let Some(top) = stack.last_mut() { top.warnings.push(name.clone()); }
                        }
                        _ => {}
                    }
                }
            }
            Event::End(e) => {
                let raw = e.name();
                let name = std::str::from_utf8(raw.as_ref()).unwrap_or("").to_string();
                if name == "RelOp" {
                    depth_since_relop.pop();
                    if let Some(done) = stack.pop() {
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(done);
                        } else {
                            root = Some(done);
                        }
                    }
                } else {
                    if let Some(d) = depth_since_relop.last_mut() { if *d > 0 { *d -= 1; } }
                    match name.as_str() {
                        "MissingIndex" => {
                            if let Some(mi) = cur_missing.take() {
                                // Prefer the enclosing RelOp; otherwise hold it
                                // until we have a root to attach to.
                                if let Some(top) = stack.last_mut() {
                                    top.missing_indexes.push(mi);
                                } else {
                                    pending_missing.push(mi);
                                }
                            }
                        }
                        "MissingIndexGroup" => { cur_group_impact = 0.0; }
                        "ColumnGroup" => { cur_usage = None; }
                        "ColumnsWithNoStatistics" => { in_no_stats = false; }
                        "DefinedValues" => { in_defined_values = false; }
                        "SeekPredicates" => { in_seek_predicates = false; }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    let mut root = root.ok_or_else(|| PlanError::Xml("no RelOp root found".into()))?;
    // Attach any missing-index recommendations that were declared at QueryPlan
    // level (outside a RelOp) to the root node so derive_findings emits them once.
    root.missing_indexes.append(&mut pending_missing);
    Ok(root)
}

pub fn derive_findings(plan: &PlanNode) -> Vec<Finding> {
    let mut out = Vec::new();
    walk(plan, None, &mut out);
    out
}

/// Find, within `node`'s direct + descendant subtree, an Index Seek operator that
/// reads the SAME base table as the lookup but via a NONCLUSTERED index — i.e. the
/// seek that feeds the Key Lookup. Returns (index_name, key_columns). Best-effort:
/// returns None when the plan shape doesn't expose a sibling seek.
fn sibling_seek_for_lookup<'a>(parent: &'a PlanNode, lookup: &PlanNode) -> Option<(&'a str, Vec<String>)> {
    fn search<'a>(n: &'a PlanNode, table: &str, lookup_index: &str) -> Option<(&'a str, Vec<String>)> {
        let op = n.physical_op.to_ascii_lowercase();
        if op.contains("index seek")
            && n.has_seek_predicate
            && n.object_table.eq_ignore_ascii_case(table)
            && !n.object_index.is_empty()
            && !n.object_index.eq_ignore_ascii_case(lookup_index)
        {
            let keys: Vec<String> = n.seek_columns.iter().map(|c| c.column.clone()).collect();
            return Some((n.object_index.as_str(), keys));
        }
        for c in &n.children {
            if let Some(r) = search(c, table, lookup_index) { return Some(r); }
        }
        None
    }
    if lookup.object_table.is_empty() { return None; }
    for c in &parent.children {
        if std::ptr::eq(c, lookup) { continue; }
        if let Some(r) = search(c, &lookup.object_table, &lookup.object_index) {
            return Some(r);
        }
    }
    None
}

/// Build the concrete covering-index recommendation for a Key/RID Lookup using the
/// REAL output columns the lookup fetches. When we can identify the sibling seek's
/// nonclustered index + its key columns, we emit a `DROP_EXISTING=ON` rebuild of
/// that exact index with the missing columns added to INCLUDE. Otherwise we emit a
/// best-effort CREATE with the lookup's join keys as the key and the real output
/// columns as INCLUDE. Returns (recommendation, include_columns) — the latter so
/// the width heuristic can re-use the same real list.
fn lookup_covering_ddl(node: &PlanNode, parent: Option<&PlanNode>) -> (String, Vec<ColumnRef>) {
    let include_cols: Vec<ColumnRef> = node.output_columns.clone();
    let schema = if node.object_schema.is_empty() { "dbo".to_string() } else { node.object_schema.clone() };
    let table = node.object_table.clone();

    let include_sql = if include_cols.is_empty() {
        "<columns currently fetched by the lookup>".to_string()
    } else {
        include_cols.iter().map(|c| bracket(&c.column)).collect::<Vec<_>>().join(", ")
    };

    // Did we find the nonclustered seek that the lookup pairs with?
    let sibling = parent.and_then(|p| sibling_seek_for_lookup(p, node));

    let ddl = if let Some((idx, keys)) = sibling.as_ref().filter(|_| !table.is_empty()) {
        let key_sql = if keys.is_empty() {
            "/* existing index key columns */".to_string()
        } else {
            keys.iter().map(|k| bracket(k)).collect::<Vec<_>>().join(", ")
        };
        // Rebuild the EXISTING nonclustered index in place with the missing
        // output columns folded into INCLUDE — the surgical, lowest-risk fix.
        format!(
            "CREATE NONCLUSTERED INDEX [{}]\n  ON [{}].[{}] ({})\n  INCLUDE ({})\n  WITH (DROP_EXISTING = ON);",
            idx, schema, table, key_sql, include_sql
        )
    } else if !table.is_empty() {
        // No sibling seek captured — emit a deployable CREATE keyed on the
        // lookup's join columns (the clustering key it probes), with the REAL
        // output columns as INCLUDE.
        let keys: Vec<String> = node.seek_columns.iter().map(|c| bracket(&c.column)).collect();
        let key_sql = if keys.is_empty() { "<seek key cols>".to_string() } else { keys.join(", ") };
        let name_part = node.seek_columns.iter().map(|c| sanitize_ident(&c.column)).collect::<Vec<_>>().join("_");
        let idx_name = if name_part.is_empty() {
            format!("IX_{}_covering", sanitize_ident(&table))
        } else {
            format!("IX_{}_{}", sanitize_ident(&table), name_part)
        };
        format!(
            "CREATE NONCLUSTERED INDEX [{}]\n  ON [{}].[{}] ({})\n  INCLUDE ({});",
            idx_name, schema, table, key_sql, include_sql
        )
    } else {
        // No object captured at all — fall back to the generic template.
        format!(
            "CREATE NONCLUSTERED INDEX IX_<table>_<keycols> ON <schema>.<table> (<seek key cols>) INCLUDE ({}) WITH (DROP_EXISTING = ON);",
            include_sql
        )
    };
    (ddl, include_cols)
}

// Thresholds tuned to fire only on plans where the shape is genuinely a problem,
// not on trivially small operators. A plan-derived finding has no source
// location, so being conservative is the only false-positive guard available.
const HIGH_ROWS: f64 = 10_000.0;
const LOOKUP_HIGH_EXEC: f64 = 1_000.0;
/// Estimate-vs-actual skew is only flagged when the larger side is at least this
/// many rows AND the ratio is extreme, to avoid noise on tiny counts.
const SKEW_MIN_ROWS: f64 = 1_000.0;
const SKEW_RATIO: f64 = 100.0;

fn walk(node: &PlanNode, parent: Option<&PlanNode>, out: &mut Vec<Finding>) {
    let loc: Option<Location> = None;

    // (a) MissingIndex -> concrete CREATE INDEX. This is the single highest-value
    // offline-plan signal: the optimizer literally tells us the covering index.
    for mi in &node.missing_indexes {
        out.push(missing_index_finding(node, mi));
    }

    if node.physical_op.eq_ignore_ascii_case("Table Scan") {
        out.push(Finding {
            rule: RuleId("plan.table_scan".into()),
            severity: Severity::Warning,
            message: format!("Table Scan in plan (NodeId={}, cost={:.4}). Heap table or no usable index.", node.node_id, node.estimated_total_subtree_cost),
            location: loc,
            recommendation: Some("Add a clustered index, or a covering nonclustered index on the predicate + included columns the query needs.".into()),
        });
    }

    // (c) Big index/table SCAN returning many rows while carrying a residual
    // predicate (rows read then discarded) — a seek was expected. We require a
    // residual predicate to fire: an unfiltered full scan (e.g. SELECT * with no
    // WHERE) is correct as a scan and must NOT be flagged.
    let is_scan = node.physical_op.eq_ignore_ascii_case("Index Scan")
        || node.physical_op.eq_ignore_ascii_case("Clustered Index Scan");
    if is_scan
        && node.has_residual_predicate
        && !node.has_seek_predicate
        && node.estimated_rows >= HIGH_ROWS
    {
        out.push(Finding {
            rule: RuleId("plan.scan_residual_predicate".into()),
            severity: Severity::Warning,
            message: format!(
                "{} at NodeId={} reads ~{:.0} rows and filters them with a residual predicate (cost={:.4}). The engine scans the whole index/table and discards non-matching rows because no index supports the predicate as a seek.",
                node.physical_op, node.node_id, node.estimated_rows, node.estimated_total_subtree_cost
            ),
            location: loc,
            recommendation: Some(
                "Create (or extend) a nonclustered index whose leading key columns match the residual predicate so the access becomes an Index Seek. Example: CREATE NONCLUSTERED INDEX IX_<table>_<cols> ON <schema>.<table> (<predicate columns>) INCLUDE (<columns the query outputs>);".into()
            ),
        });
    }

    // (d) Key/RID Lookup. Always worth surfacing; escalate severity when the
    // estimated executions are high (the lookup runs per outer row, so a covering
    // index removes thousands of random IOs).
    if node.physical_op.eq_ignore_ascii_case("Key Lookup") || node.physical_op.eq_ignore_ascii_case("RID Lookup") {
        let high = node.estimated_executions >= LOOKUP_HIGH_EXEC
            || node.actual_executions.map(|x| x >= LOOKUP_HIGH_EXEC).unwrap_or(false);
        let exec_txt = if let Some(a) = node.actual_executions {
            format!("~{:.0} actual executions", a)
        } else {
            format!("~{:.0} estimated executions", node.estimated_executions)
        };
        let sev = if high { Severity::Error } else { Severity::Warning };
        let (ddl, include_cols) = lookup_covering_ddl(node, parent);
        // Name the real output columns inline so the message is self-explanatory.
        let cols_txt = if include_cols.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = include_cols.iter().map(|c| c.column.clone()).collect();
            format!(" It outputs {} column(s) not in that index: {}.", names.len(), names.join(", "))
        };
        out.push(Finding {
            rule: RuleId("plan.lookup".into()),
            severity: sev,
            message: format!(
                "{} in plan (NodeId={}, cost={:.4}, {}). The nonclustered index used by the seek does not cover the query, so the engine performs a per-row lookup into the base table.{}{}",
                node.physical_op, node.node_id, node.estimated_total_subtree_cost, exec_txt,
                if high { " Run per outer row at high volume, this is often the dominant cost in the plan." } else { "" },
                cols_txt
            ),
            location: loc,
            recommendation: Some(format!(
                "Make the seeking index covering by adding the columns the lookup fetches as INCLUDE columns, then verify the plan picks it and the extra write cost is acceptable before shipping:\n{}",
                ddl
            )),
        });

        // Wide-covering-request caveat: a very wide INCLUDE list (many columns)
        // bloats the nonclustered leaf and slows every write to the table. From a
        // plan alone we know column COUNT but not types, so the plan-side caveat is
        // count-only; the source-DDL rule (index.wide_covering_request, registered
        // via ss(...)) additionally catches large/LOB types in pasted CREATE INDEX.
        const WIDE_INCLUDE_COUNT: usize = 5;
        if include_cols.len() > WIDE_INCLUDE_COUNT {
            out.push(Finding {
                rule: RuleId("index.wide_covering_request".into()),
                severity: Severity::Info,
                message: format!(
                    "Write-amplification caveat at NodeId={}: the covering index that removes this lookup would INCLUDE {} columns. Every INCLUDE column is duplicated into the nonclustered index leaf, so the index grows and every INSERT/UPDATE/DELETE that touches those columns pays to maintain the copy.",
                    node.node_id, include_cols.len()
                ),
                location: loc,
                recommendation: Some(
                    "Add INCLUDE columns deliberately, not reflexively. Prefer covering only the columns this query actually returns, drop wide/LOB columns from INCLUDE if the lookup is rare, or accept the lookup when the table is write-heavy. Measure read benefit against the added write + storage cost before shipping.".into()
                ),
            });
        }
    }

    // (b) Promote well-known plan warnings, now with the captured detail.
    for w in &node.warnings {
        let f = match w.as_str() {
            "PlanAffectingConvert" => {
                let detail = node.convert.as_ref();
                let issue = detail.map(|c| c.issue.as_str()).filter(|s| !s.is_empty());
                let expr = detail.map(|c| c.expression.as_str()).filter(|s| !s.is_empty());
                let msg = match (issue, expr) {
                    (Some(i), Some(x)) => format!(
                        "Implicit data-type conversion at NodeId={} (PlanAffectingConvert, {}): {}. The CONVERT_IMPLICIT on the column side defeats an index seek and skews cardinality.",
                        node.node_id, i, x
                    ),
                    _ => format!(
                        "Implicit data-type conversion at NodeId={} (PlanAffectingConvert): the optimizer is converting a column to match the other side of a comparison, which can defeat an index seek and produce bad cardinality estimates.",
                        node.node_id
                    ),
                };
                Finding {
                    rule: RuleId("plan.implicit_conversion".into()),
                    severity: Severity::Error,
                    message: msg,
                    location: loc,
                    recommendation: Some("Make the predicate's types match the column exactly (parameter/literal type, collation). Fix the type — not the index. e.g. declare @p the same type as the column, or cast the *parameter* side, never the column.".into()),
                }
            }
            "NoJoinPredicate" => Finding {
                rule: RuleId("plan.missing_join_predicate".into()),
                severity: Severity::Error,
                message: format!("Missing join predicate at NodeId={} (NoJoinPredicate): two inputs are joined with no ON/WHERE relating them — a Cartesian product. Row counts multiply and the query explodes.", node.node_id),
                location: loc,
                recommendation: Some("Add the join condition (ON a.key = b.key). If a cross join is genuinely intended, write CROSS JOIN explicitly so the intent is unmistakable.".into()),
            },
            "SpillToTempDb" => Finding {
                rule: RuleId("plan.spill".into()),
                severity: Severity::Warning,
                message: format!("Operator spills to tempdb at NodeId={} (SpillToTempDb): the memory grant was too small for the sort/hash, so it spilled to disk — slow IO and tempdb pressure under load.", node.node_id),
                location: loc,
                recommendation: Some("Reduce the rowset reached by the sort/hash (filter earlier, paginate), add a supporting index to avoid the sort, or fix the cardinality estimate (stats) that under-sized the grant.".into()),
            },
            "ColumnsWithNoStatistics" => {
                let cols: Vec<String> = node.no_stats_columns.iter()
                    .filter(|c| !c.column.is_empty())
                    .map(|c| if c.table.is_empty() { bracket(&c.column) } else { format!("{}.{}", bracket(&c.table), bracket(&c.column)) })
                    .collect();
                let (msg, rec) = if cols.is_empty() {
                    (
                        format!("Columns with no statistics at NodeId={}: the optimizer had to guess cardinality with no histogram, which routinely yields the wrong plan shape.", node.node_id),
                        "CREATE STATISTICS on the flagged column(s), or enable AUTO_CREATE_STATISTICS. Verify with actual-vs-estimated rows once stats exist.".to_string(),
                    )
                } else {
                    (
                        format!("Columns with no statistics at NodeId={}: {} — the optimizer guessed cardinality with no histogram, which routinely yields the wrong plan shape.", node.node_id, cols.join(", ")),
                        format!("Create statistics so the optimizer can estimate, then re-check actual-vs-estimated rows. e.g. {}", create_stats_sql(&node.no_stats_columns)),
                    )
                };
                Finding {
                    rule: RuleId("plan.missing_statistics".into()),
                    severity: Severity::Warning,
                    message: msg,
                    location: loc,
                    recommendation: Some(rec),
                }
            }
            other => Finding {
                rule: RuleId(format!("plan.warning.{}", other)),
                severity: Severity::Warning,
                message: format!("Plan warning at NodeId={}: {}", node.node_id, other),
                location: loc,
                recommendation: None,
            },
        };
        out.push(f);
    }

    // (e) Estimated-vs-actual cardinality skew (actual plans only). A blown
    // estimate is the root cause of most bad plans (wrong joins, tiny grants).
    if let Some(actual) = node.actual_rows {
        let est = node.estimated_rows;
        let big = actual.max(est);
        if big >= SKEW_MIN_ROWS {
            // Guard against divide-by-zero; treat <1 as 1 row for the ratio.
            let lo = est.min(actual).max(1.0);
            let hi = big.max(1.0);
            if hi / lo >= SKEW_RATIO {
                let dir = if actual > est { "UNDER" } else { "OVER" };
                out.push(Finding {
                    rule: RuleId("plan.estimate_actual_skew".into()),
                    severity: Severity::Error,
                    message: format!(
                        "Cardinality estimate is off by {:.0}x at NodeId={} ({} op): estimated {:.0} rows, actual {:.0} rows ({}-estimated). The optimizer sized this plan for the wrong row count.",
                        hi / lo, node.node_id, node.physical_op, est, actual, dir
                    ),
                    location: loc,
                    recommendation: Some(
                        "Find the source of the bad estimate: stale/missing statistics (UPDATE STATISTICS or CREATE STATISTICS), a non-sargable or implicitly-converted predicate, a multi-statement table-valued or scalar UDF (fixed-guess cardinality), or a local variable the optimizer can't sniff (try OPTION (RECOMPILE) to confirm). Fix the estimate and the plan shape, grant, and joins follow.".into()
                    ),
                });
            }
        }
    }

    for c in &node.children {
        walk(c, Some(node), out);
    }
}

/// Build the `CREATE NONCLUSTERED INDEX` that satisfies a `<MissingIndex>` node,
/// following the optimizer's own column ordering (equality, then inequality,
/// then INCLUDE). Identifiers are bracket-quoted.
fn missing_index_finding(node: &PlanNode, mi: &MissingIndexInfo) -> Finding {
    let schema = if mi.schema.is_empty() { "dbo".to_string() } else { mi.schema.clone() };
    let table = if mi.table.is_empty() { "<table>".to_string() } else { mi.table.clone() };

    // Key = equality columns then inequality columns (the standard recipe).
    let mut key_cols: Vec<String> = Vec::new();
    key_cols.extend(mi.equality.iter().map(|c| bracket(c)));
    key_cols.extend(mi.inequality.iter().map(|c| bracket(c)));
    let include_cols: Vec<String> = mi.included.iter().map(|c| bracket(c)).collect();

    // Index name from the key columns, sanitized.
    let name_part: String = mi.equality.iter().chain(mi.inequality.iter())
        .map(|c| unbracket(c)).collect::<Vec<_>>().join("_");
    let name_part = sanitize_ident(&name_part);
    let idx_name = if name_part.is_empty() {
        format!("IX_{}", sanitize_ident(&table))
    } else {
        format!("IX_{}_{}", sanitize_ident(&table), name_part)
    };

    let key_sql = if key_cols.is_empty() { "/* no key columns reported */".to_string() } else { key_cols.join(", ") };
    let mut ddl = format!(
        "CREATE NONCLUSTERED INDEX [{}]\n  ON [{}].[{}] ({})",
        idx_name, schema, table, key_sql
    );
    if !include_cols.is_empty() {
        ddl.push_str(&format!("\n  INCLUDE ({})", include_cols.join(", ")));
    }
    ddl.push(';');

    let impact_txt = if mi.impact > 0.0 { format!(" estimated impact {:.0}%", mi.impact) } else { String::new() };

    Finding {
        rule: RuleId("plan.missing_index".into()),
        severity: Severity::Warning,
        message: format!(
            "The plan reports a missing index on [{}].[{}] (NodeId={},{}). The optimizer would prefer a covering index here.",
            schema, table, node.node_id, impact_txt
        ),
        location: None,
        recommendation: Some(format!(
            "Create the index the optimizer asked for, then re-check the plan (verify it is actually picked and the write cost is acceptable before shipping):\n{}",
            ddl
        )),
    }
}

/// Emit one or more `CREATE STATISTICS` statements for the no-stats columns.
fn create_stats_sql(cols: &[ColumnRef]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in cols {
        if c.column.is_empty() { continue; }
        if c.table.is_empty() {
            parts.push(format!("CREATE STATISTICS ST_{} ON <table> ({});", sanitize_ident(&c.column), bracket(&c.column)));
        } else {
            parts.push(format!(
                "CREATE STATISTICS ST_{}_{} ON {} ({});",
                sanitize_ident(&c.table), sanitize_ident(&c.column), bracket(&c.table), bracket(&c.column)
            ));
        }
    }
    parts.join(" ")
}

/// Reduce an identifier to a name fragment safe for an index/statistics name.
fn sanitize_ident(s: &str) -> String {
    unbracket(s).chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(plan: &PlanNode) -> Vec<String> {
        derive_findings(plan).into_iter().map(|f| f.rule.0).collect()
    }

    fn fired(plan: &PlanNode, id: &str) -> Vec<Finding> {
        derive_findings(plan).into_iter().filter(|f| f.rule.0 == id).collect()
    }

    // ---------------------------------------------------------------------
    // (a) plan.missing_index
    // ---------------------------------------------------------------------
    const MISSING_INDEX_PLAN: &str = r#"
    <ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple>
      <QueryPlan>
        <MissingIndexes>
          <MissingIndexGroup Impact="92.7117">
            <MissingIndex Database="[app]" Schema="[dbo]" Table="[Orders]">
              <ColumnGroup Usage="EQUALITY">
                <Column Name="[CustomerId]" ColumnId="2" />
              </ColumnGroup>
              <ColumnGroup Usage="INEQUALITY">
                <Column Name="[OrderDate]" ColumnId="3" />
              </ColumnGroup>
              <ColumnGroup Usage="INCLUDE">
                <Column Name="[Total]" ColumnId="5" />
                <Column Name="[Status]" ColumnId="6" />
              </ColumnGroup>
            </MissingIndex>
          </MissingIndexGroup>
        </MissingIndexes>
        <RelOp NodeId="0" PhysicalOp="Clustered Index Scan" LogicalOp="Clustered Index Scan"
               EstimateRows="500000" EstimatedTotalSubtreeCost="12.5">
          <IndexScan>
            <Predicate><ScalarOperator ScalarString="x" /></Predicate>
          </IndexScan>
        </RelOp>
      </QueryPlan>
    </StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

    #[test]
    fn missing_index_fires_with_create_index_ddl() {
        let plan = parse(MISSING_INDEX_PLAN).expect("parse");
        let f = fired(&plan, "plan.missing_index");
        assert_eq!(f.len(), 1, "exactly one missing-index finding");
        let rec = f[0].recommendation.as_ref().expect("rec");
        // Concrete, correctly-ordered DDL.
        assert!(rec.contains("CREATE NONCLUSTERED INDEX"), "rec has CREATE: {rec}");
        assert!(rec.contains("[dbo].[Orders]"), "schema.table: {rec}");
        // equality then inequality in the key
        let key_pos_cust = rec.find("[CustomerId]").unwrap();
        let key_pos_date = rec.find("[OrderDate]").unwrap();
        assert!(key_pos_cust < key_pos_date, "equality before inequality");
        assert!(rec.contains("INCLUDE ([Total], [Status])"), "include cols: {rec}");
        assert!(f[0].message.contains("93%"), "impact surfaced: {}", f[0].message);
    }

    #[test]
    fn missing_index_negative_clean_seek_plan() {
        // A plan with a healthy Index Seek and no MissingIndexes element must
        // produce no plan.missing_index finding.
        let xml = r#"<ShowPlanXML><RelOp NodeId="0" PhysicalOp="Index Seek" LogicalOp="Index Seek"
            EstimateRows="3" EstimatedTotalSubtreeCost="0.003">
            <IndexScan><SeekPredicates><SeekPredicateNew /></SeekPredicates></IndexScan>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        assert!(!ids(&plan).iter().any(|i| i == "plan.missing_index"));
    }

    // ---------------------------------------------------------------------
    // (b) plan.implicit_conversion / plan.missing_statistics detail
    // ---------------------------------------------------------------------
    #[test]
    fn implicit_conversion_with_expression_detail() {
        let xml = r#"<ShowPlanXML><RelOp NodeId="1" PhysicalOp="Index Scan" LogicalOp="Index Scan"
            EstimateRows="10" EstimatedTotalSubtreeCost="0.01">
            <Warnings>
              <PlanAffectingConvert ConvertIssue="Cardinality Estimate"
                Expression="CONVERT_IMPLICIT(int,[t].[code],0)=[@p]" />
            </Warnings>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        let f = fired(&plan, "plan.implicit_conversion");
        assert_eq!(f.len(), 1);
        assert!(matches!(f[0].severity, Severity::Error));
        assert!(f[0].message.contains("CONVERT_IMPLICIT"), "expr surfaced: {}", f[0].message);
        assert!(f[0].message.contains("Cardinality Estimate"));
    }

    #[test]
    fn missing_statistics_lists_columns_and_emits_create_statistics() {
        let xml = r#"<ShowPlanXML><RelOp NodeId="2" PhysicalOp="Table Scan" LogicalOp="Table Scan"
            EstimateRows="100" EstimatedTotalSubtreeCost="1.0">
            <Warnings>
              <ColumnsWithNoStatistics>
                <ColumnReference Database="[app]" Schema="[dbo]" Table="[Sales]" Column="[Region]" />
              </ColumnsWithNoStatistics>
            </Warnings>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        let f = fired(&plan, "plan.missing_statistics");
        assert_eq!(f.len(), 1, "one missing-stats finding");
        assert!(f[0].message.contains("[Region]"), "col named: {}", f[0].message);
        let rec = f[0].recommendation.as_ref().unwrap();
        assert!(rec.contains("CREATE STATISTICS"), "rec: {rec}");
        assert!(rec.contains("[Sales]"));
    }

    #[test]
    fn warnings_negative_clean_plan_no_warning_findings() {
        let xml = r#"<ShowPlanXML><RelOp NodeId="0" PhysicalOp="Index Seek" LogicalOp="Index Seek"
            EstimateRows="2" EstimatedTotalSubtreeCost="0.002">
            <IndexScan><SeekPredicates /></IndexScan>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        let got = ids(&plan);
        assert!(!got.iter().any(|i| i.starts_with("plan.implicit_conversion")));
        assert!(!got.iter().any(|i| i == "plan.missing_statistics"));
        assert!(!got.iter().any(|i| i.starts_with("plan.warning")));
    }

    // ---------------------------------------------------------------------
    // (c) plan.scan_residual_predicate
    // ---------------------------------------------------------------------
    #[test]
    fn big_scan_with_residual_predicate_fires() {
        let xml = r#"<ShowPlanXML><RelOp NodeId="3" PhysicalOp="Index Scan" LogicalOp="Index Scan"
            EstimateRows="250000" EstimatedTotalSubtreeCost="8.4">
            <IndexScan>
              <Predicate><ScalarOperator ScalarString="[t].[name] like '%x%'" /></Predicate>
            </IndexScan>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        let f = fired(&plan, "plan.scan_residual_predicate");
        assert_eq!(f.len(), 1);
        assert!(f[0].recommendation.as_ref().unwrap().contains("Index Seek"));
    }

    #[test]
    fn small_scan_does_not_fire_residual() {
        // Same shape but few rows: a tiny residual scan is fine, must NOT fire.
        let xml = r#"<ShowPlanXML><RelOp NodeId="3" PhysicalOp="Index Scan" LogicalOp="Index Scan"
            EstimateRows="42" EstimatedTotalSubtreeCost="0.05">
            <IndexScan><Predicate><ScalarOperator ScalarString="x" /></Predicate></IndexScan>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        assert!(!ids(&plan).iter().any(|i| i == "plan.scan_residual_predicate"));
    }

    #[test]
    fn big_scan_without_residual_does_not_fire() {
        // A big full scan with NO residual predicate (e.g. SELECT * with no
        // WHERE) is a correct scan — must NOT fire.
        let xml = r#"<ShowPlanXML><RelOp NodeId="3" PhysicalOp="Clustered Index Scan" LogicalOp="Clustered Index Scan"
            EstimateRows="900000" EstimatedTotalSubtreeCost="22.0">
            <IndexScan />
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        assert!(!ids(&plan).iter().any(|i| i == "plan.scan_residual_predicate"));
    }

    #[test]
    fn seek_with_residual_does_not_fire_scan_rule() {
        // A seek that also carries a residual predicate is still a seek — the
        // SeekPredicates guard must keep the scan rule quiet.
        let xml = r#"<ShowPlanXML><RelOp NodeId="3" PhysicalOp="Index Seek" LogicalOp="Index Seek"
            EstimateRows="300000" EstimatedTotalSubtreeCost="5.0">
            <IndexScan>
              <SeekPredicates><SeekPredicateNew /></SeekPredicates>
              <Predicate><ScalarOperator ScalarString="x" /></Predicate>
            </IndexScan>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        assert!(!ids(&plan).iter().any(|i| i == "plan.scan_residual_predicate"));
    }

    // ---------------------------------------------------------------------
    // (d) plan.lookup escalation
    // ---------------------------------------------------------------------
    #[test]
    fn key_lookup_high_executions_is_error_with_covering_ddl() {
        let xml = r#"<ShowPlanXML>
          <RelOp NodeId="0" PhysicalOp="Nested Loops" LogicalOp="Inner Join"
                 EstimateRows="50000" EstimatedTotalSubtreeCost="40.0">
            <RelOp NodeId="2" PhysicalOp="Key Lookup" LogicalOp="Clustered Index Seek"
                   EstimateRows="1" EstimateRebinds="49999" EstimateRewinds="0"
                   EstimatedTotalSubtreeCost="35.0">
              <IndexScan><SeekPredicates /></IndexScan>
            </RelOp>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        let f = fired(&plan, "plan.lookup");
        assert_eq!(f.len(), 1);
        assert!(matches!(f[0].severity, Severity::Error), "high-exec lookup is Error");
        assert!(f[0].recommendation.as_ref().unwrap().contains("INCLUDE"));
    }

    // A realistic Nested Loops over a nonclustered Index Seek + Key Lookup. The
    // lookup carries DefinedValues (its real output columns) and an Object naming
    // the clustered index it probes; the sibling seek names the nonclustered
    // index + its key columns. The recommendation must be CONCRETE, not a
    // placeholder, and must rebuild the sibling index with the real INCLUDE list.
    const KEY_LOOKUP_REAL_COLS: &str = r#"
    <ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple><QueryPlan>
      <RelOp NodeId="0" PhysicalOp="Nested Loops" LogicalOp="Inner Join"
             EstimateRows="50000" EstimatedTotalSubtreeCost="40.0">
        <RelOp NodeId="1" PhysicalOp="Index Seek" LogicalOp="Index Seek"
               EstimateRows="50000" EstimatedTotalSubtreeCost="2.0">
          <IndexScan>
            <Object Database="[app]" Schema="[dbo]" Table="[Orders]" Index="[IX_Orders_CustomerId]" />
            <SeekPredicates>
              <SeekPredicateNew>
                <SeekKeys><Prefix><RangeColumns>
                  <ColumnReference Database="[app]" Schema="[dbo]" Table="[Orders]" Column="[CustomerId]" />
                </RangeColumns></Prefix></SeekKeys>
              </SeekPredicateNew>
            </SeekPredicates>
          </IndexScan>
        </RelOp>
        <RelOp NodeId="2" PhysicalOp="Key Lookup" LogicalOp="Clustered Index Seek"
               EstimateRows="1" EstimateRebinds="49999" EstimateRewinds="0"
               EstimatedTotalSubtreeCost="35.0">
          <IndexScan Lookup="1">
            <DefinedValues>
              <DefinedValue><ColumnReference Database="[app]" Schema="[dbo]" Table="[Orders]" Column="[Total]" /></DefinedValue>
              <DefinedValue><ColumnReference Database="[app]" Schema="[dbo]" Table="[Orders]" Column="[Status]" /></DefinedValue>
            </DefinedValues>
            <Object Database="[app]" Schema="[dbo]" Table="[Orders]" Index="[PK_Orders]" />
            <SeekPredicates>
              <SeekPredicateNew>
                <SeekKeys><Prefix><RangeColumns>
                  <ColumnReference Database="[app]" Schema="[dbo]" Table="[Orders]" Column="[OrderId]" />
                </RangeColumns></Prefix></SeekKeys>
              </SeekPredicateNew>
            </SeekPredicates>
          </IndexScan>
        </RelOp>
      </RelOp>
    </QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

    #[test]
    fn key_lookup_emits_concrete_covering_ddl_with_real_columns() {
        let plan = parse(KEY_LOOKUP_REAL_COLS).expect("parse");
        let f = fired(&plan, "plan.lookup");
        assert_eq!(f.len(), 1, "exactly one lookup finding");
        let rec = f[0].recommendation.as_ref().expect("rec");
        // Concrete, deployable DDL — no <placeholder> tokens.
        assert!(rec.contains("CREATE NONCLUSTERED INDEX"), "has CREATE: {rec}");
        assert!(rec.contains("[dbo].[Orders]"), "real schema.table: {rec}");
        // Rebuilds the sibling nonclustered index in place...
        assert!(rec.contains("[IX_Orders_CustomerId]"), "sibling index name: {rec}");
        assert!(rec.contains("DROP_EXISTING = ON"), "in-place rebuild: {rec}");
        // ...keyed on the seek key column...
        assert!(rec.contains("([CustomerId])"), "key column: {rec}");
        // ...with the REAL lookup output columns as INCLUDE.
        assert!(rec.contains("INCLUDE ([Total], [Status])"), "real INCLUDE cols: {rec}");
        // No placeholder leaked through.
        assert!(!rec.contains("<columns"), "no placeholder include: {rec}");
        // Message names the missing columns.
        assert!(f[0].message.contains("Total") && f[0].message.contains("Status"), "msg names cols: {}", f[0].message);
    }

    #[test]
    fn key_lookup_wide_include_emits_write_amplification_caveat() {
        // Seven output columns on the lookup -> index.wide_covering_request fires.
        let mut dvs = String::new();
        for c in ["A","B","C","D","E","F","G"] {
            dvs.push_str(&format!(
                "<DefinedValue><ColumnReference Schema=\"[dbo]\" Table=\"[Wide]\" Column=\"[{c}]\" /></DefinedValue>"
            ));
        }
        let xml = format!(r#"<ShowPlanXML><RelOp NodeId="2" PhysicalOp="Key Lookup" LogicalOp="Clustered Index Seek"
            EstimateRows="1" EstimateRebinds="49999" EstimateRewinds="0" EstimatedTotalSubtreeCost="35.0">
            <IndexScan Lookup="1">
              <DefinedValues>{dvs}</DefinedValues>
              <Object Schema="[dbo]" Table="[Wide]" Index="[PK_Wide]" />
              <SeekPredicates><SeekPredicateNew><SeekKeys><Prefix><RangeColumns>
                <ColumnReference Schema="[dbo]" Table="[Wide]" Column="[Id]" />
              </RangeColumns></Prefix></SeekKeys></SeekPredicateNew></SeekPredicates>
            </IndexScan>
          </RelOp></ShowPlanXML>"#);
        let plan = parse(&xml).expect("parse");
        let caveat = fired(&plan, "index.wide_covering_request");
        assert_eq!(caveat.len(), 1, "wide-covering caveat fires");
        assert!(matches!(caveat[0].severity, Severity::Info), "caveat is Info");
        assert!(caveat[0].message.contains("7 columns"), "names the width: {}", caveat[0].message);
    }

    #[test]
    fn key_lookup_narrow_include_no_wide_caveat() {
        // The realistic 2-column plan must NOT raise the wide-covering caveat.
        let plan = parse(KEY_LOOKUP_REAL_COLS).expect("parse");
        assert!(fired(&plan, "index.wide_covering_request").is_empty(),
            "narrow INCLUDE list must not trip the write-amplification caveat");
    }

    #[test]
    fn key_lookup_low_executions_is_warning_not_error() {
        let xml = r#"<ShowPlanXML><RelOp NodeId="2" PhysicalOp="Key Lookup" LogicalOp="Clustered Index Seek"
            EstimateRows="1" EstimateRebinds="3" EstimateRewinds="0" EstimatedTotalSubtreeCost="0.01">
            <IndexScan><SeekPredicates /></IndexScan>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        let f = fired(&plan, "plan.lookup");
        assert_eq!(f.len(), 1);
        assert!(matches!(f[0].severity, Severity::Warning), "low-exec lookup stays Warning");
    }

    // ---------------------------------------------------------------------
    // (e) plan.estimate_actual_skew
    // ---------------------------------------------------------------------
    #[test]
    fn estimate_actual_skew_fires_on_extreme_underestimate() {
        let xml = r#"<ShowPlanXML><RelOp NodeId="4" PhysicalOp="Index Seek" LogicalOp="Index Seek"
            EstimateRows="5" EstimatedTotalSubtreeCost="0.01">
            <IndexScan><SeekPredicates /></IndexScan>
            <RunTimeInformation>
              <RunTimeCountersPerThread ActualRows="500000" ActualExecutions="1" />
            </RunTimeInformation>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        let f = fired(&plan, "plan.estimate_actual_skew");
        assert_eq!(f.len(), 1);
        assert!(matches!(f[0].severity, Severity::Error));
        assert!(f[0].message.contains("UNDER-estimated"));
    }

    #[test]
    fn estimate_actual_skew_quiet_on_estimated_only_plan() {
        // No RunTimeInformation -> no actual rows -> rule must stay silent even
        // when EstimateRows is large.
        let xml = r#"<ShowPlanXML><RelOp NodeId="4" PhysicalOp="Index Seek" LogicalOp="Index Seek"
            EstimateRows="500000" EstimatedTotalSubtreeCost="6.0">
            <IndexScan><SeekPredicates /></IndexScan>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        assert!(!ids(&plan).iter().any(|i| i == "plan.estimate_actual_skew"));
    }

    #[test]
    fn estimate_actual_skew_quiet_on_small_or_accurate_counts() {
        // Accurate estimate at high volume -> no skew.
        let xml = r#"<ShowPlanXML><RelOp NodeId="4" PhysicalOp="Index Seek" LogicalOp="Index Seek"
            EstimateRows="100000" EstimatedTotalSubtreeCost="2.0">
            <IndexScan><SeekPredicates /></IndexScan>
            <RunTimeInformation>
              <RunTimeCountersPerThread ActualRows="98000" ActualExecutions="1" />
            </RunTimeInformation>
          </RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        assert!(!ids(&plan).iter().any(|i| i == "plan.estimate_actual_skew"));
    }

    // ---------------------------------------------------------------------
    // Regression: existing behavior preserved.
    // ---------------------------------------------------------------------
    #[test]
    fn existing_table_scan_and_parse_still_work() {
        let xml = r#"<ShowPlanXML><RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan"
            EstimateRows="5" EstimatedTotalSubtreeCost="0.5"></RelOp></ShowPlanXML>"#;
        let plan = parse(xml).expect("parse");
        assert!(ids(&plan).iter().any(|i| i == "plan.table_scan"));
    }

    #[test]
    fn parse_error_on_garbage() {
        assert!(parse("<not-a-plan/>").is_err());
    }
}
