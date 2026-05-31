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
                        "Predicate" => {
                            if cur_depth == 1 || cur_depth == 2 {
                                if let Some(top) = stack.last_mut() { top.has_residual_predicate = true; }
                            }
                        }
                        "Column" | "ColumnReference" => {
                            let col = unbracket(&attr_val(&e, "Column")
                                .or_else(|| attr_val(&e, "Name"))
                                .unwrap_or_default());
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
    walk(plan, &mut out);
    out
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

fn walk(node: &PlanNode, out: &mut Vec<Finding>) {
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
        out.push(Finding {
            rule: RuleId("plan.lookup".into()),
            severity: sev,
            message: format!(
                "{} in plan (NodeId={}, cost={:.4}, {}). The nonclustered index used by the seek does not cover the query, so the engine performs a per-row lookup into the base table.{}",
                node.physical_op, node.node_id, node.estimated_total_subtree_cost, exec_txt,
                if high { " Run per outer row at high volume, this is often the dominant cost in the plan." } else { "" }
            ),
            location: loc,
            recommendation: Some(
                "Make the seeking index covering: add the query's output (SELECT-list) columns to that nonclustered index as INCLUDE columns. Example: CREATE NONCLUSTERED INDEX IX_<table>_<keycols> ON <schema>.<table> (<seek key cols>) INCLUDE (<columns currently fetched by the lookup>) WITH (DROP_EXISTING = ON);".into()
            ),
        });
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
        walk(c, out);
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
