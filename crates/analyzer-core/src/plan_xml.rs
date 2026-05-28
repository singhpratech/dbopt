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

pub fn parse(xml: &str) -> Result<PlanNode, PlanError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut stack: Vec<PlanNode> = Vec::new();
    let mut root: Option<PlanNode> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf).map_err(|e| PlanError::Xml(e.to_string()))? {
            Event::Eof => break,
            Event::Start(e) => {
                let raw = e.name();
                let name = std::str::from_utf8(raw.as_ref()).unwrap_or("").to_string();
                if name == "RelOp" {
                    let mut node = PlanNode::default();
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
                            _ => {}
                        }
                    }
                    stack.push(node);
                } else if is_warning_elem(&name) {
                    // A warning element (possibly with children, e.g.
                    // ColumnsWithNoStatistics). The <Warnings> container itself
                    // is NOT a warning — skip it.
                    if let Some(top) = stack.last_mut() {
                        top.warnings.push(name);
                    }
                }
            }
            Event::Empty(e) => {
                let raw = e.name();
                let name = std::str::from_utf8(raw.as_ref()).unwrap_or("").to_string();
                if name == "RelOp" {
                    let mut node = PlanNode::default();
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
                            _ => {}
                        }
                    }
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    } else {
                        // Empty root RelOp (rare but possible)
                        root = Some(node);
                    }
                } else if is_warning_elem(&name) {
                    if let Some(top) = stack.last_mut() {
                        top.warnings.push(name);
                    }
                }
            }
            Event::End(e) => {
                let raw = e.name();
                let name = std::str::from_utf8(raw.as_ref()).unwrap_or("");
                if name == "RelOp" {
                    if let Some(done) = stack.pop() {
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(done);
                        } else {
                            root = Some(done);
                        }
                    }
                }
            }
            _ => {}
        }
        buf.clear();
    }

    root.ok_or_else(|| PlanError::Xml("no RelOp root found".into()))
}

pub fn derive_findings(plan: &PlanNode) -> Vec<Finding> {
    let mut out = Vec::new();
    walk(plan, &mut out);
    out
}

fn walk(node: &PlanNode, out: &mut Vec<Finding>) {
    let loc: Option<Location> = None;
    if node.physical_op.eq_ignore_ascii_case("Table Scan") {
        out.push(Finding {
            rule: RuleId("plan.table_scan".into()),
            severity: Severity::Warning,
            message: format!("Table Scan in plan (NodeId={}, cost={:.4}). Heap table or no usable index.", node.node_id, node.estimated_total_subtree_cost),
            location: loc,
            recommendation: Some("Add a clustered index, or a covering nonclustered index on the predicate + included columns the query needs.".into()),
        });
    }
    if node.physical_op.eq_ignore_ascii_case("Key Lookup") || node.physical_op.eq_ignore_ascii_case("RID Lookup") {
        out.push(Finding {
            rule: RuleId("plan.lookup".into()),
            severity: Severity::Warning,
            message: format!("{} in plan (NodeId={}, cost={:.4}). Nonclustered index is missing INCLUDEd columns the query needs.", node.physical_op, node.node_id, node.estimated_total_subtree_cost),
            location: loc,
            recommendation: Some("Add the SELECT-list / output columns to the existing nonclustered index as INCLUDE columns to make it covering. Validate the rewrite avoids over-widening the index.".into()),
        });
    }
    for w in &node.warnings {
        // Promote the well-known plan warnings to first-class, prescriptive
        // rules. Unknown warnings keep the generic `plan.warning.<name>` id.
        let (rule, severity, message, rec): (&str, Severity, String, Option<String>) = match w.as_str() {
            "PlanAffectingConvert" => (
                "plan.implicit_conversion",
                Severity::Error,
                format!("Implicit data-type conversion at NodeId={} (PlanAffectingConvert): the optimizer is converting a column to match the other side of a comparison, which can defeat an index seek and produce bad cardinality estimates.", node.node_id),
                Some("Make the predicate's types match the column exactly (parameter/literal type, collation). The CONVERT_IMPLICIT on the column side is what turns a seek into a scan — fix the type, not the index.".into()),
            ),
            "NoJoinPredicate" => (
                "plan.missing_join_predicate",
                Severity::Error,
                format!("Missing join predicate at NodeId={} (NoJoinPredicate): two inputs are joined with no ON/WHERE relating them — a Cartesian product. Row counts multiply and the query explodes.", node.node_id),
                Some("Add the join condition (ON a.key = b.key). If a cross join is genuinely intended, write CROSS JOIN explicitly so the intent is unmistakable.".into()),
            ),
            "SpillToTempDb" => (
                "plan.spill",
                Severity::Warning,
                format!("Operator spills to tempdb at NodeId={} (SpillToTempDb): the memory grant was too small for the sort/hash, so it spilled to disk — slow IO and tempdb pressure under load.", node.node_id),
                Some("Reduce the rowset reached by the sort/hash (filter earlier, paginate), add a supporting index to avoid the sort, or fix the cardinality estimate (stats) that under-sized the grant.".into()),
            ),
            "ColumnsWithNoStatistics" => (
                "plan.missing_statistics",
                Severity::Warning,
                format!("Columns with no statistics at NodeId={}: the optimizer had to guess cardinality with no histogram, which routinely yields the wrong plan shape.", node.node_id),
                Some("CREATE STATISTICS on the flagged column(s), or enable AUTO_CREATE_STATISTICS. Verify with actual-vs-estimated rows once stats exist.".into()),
            ),
            other => (
                "plan.warning",
                Severity::Warning,
                format!("Plan warning at NodeId={}: {}", node.node_id, other),
                None,
            ),
        };
        let rule = if rule == "plan.warning" {
            RuleId(format!("plan.warning.{}", w))
        } else {
            RuleId(rule.into())
        };
        out.push(Finding { rule, severity, message, location: loc, recommendation: rec });
    }
    for c in &node.children {
        walk(c, out);
    }
}
