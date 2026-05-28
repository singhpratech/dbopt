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
                } else if name == "Warnings" || name.ends_with("Warning") {
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
                } else if matches!(name.as_str(), "NoJoinPredicate" | "ColumnsWithNoStatistics" | "PlanAffectingConvert" | "SpillToTempDb" | "MemoryGrantWarning") {
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
        out.push(Finding {
            rule: RuleId(format!("plan.warning.{}", w)),
            severity: Severity::Warning,
            message: format!("Plan warning at NodeId={}: {}", node.node_id, w),
            location: loc,
            recommendation: None,
        });
    }
    for c in &node.children {
        walk(c, out);
    }
}
