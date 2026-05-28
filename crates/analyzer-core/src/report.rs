use crate::findings::{Finding, Severity};
use crate::plan_xml::PlanNode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub findings: Vec<Finding>,
    pub charts: ChartData,
    /// Ranked, prescriptive remediation with copy-paste T-SQL (from the DMV
    /// recommendation engine). Empty unless a DMV bundle was provided.
    #[serde(default)]
    pub recommendations: Vec<crate::dmv::Recommendation>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ChartData {
    pub plan_treemap: Vec<TreemapNode>,
    pub index_heatmap: Vec<HeatmapCell>,
    pub size_treemap: Vec<SizeNode>,
    pub severity_timeline: Vec<SeverityBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreemapNode {
    pub name: String,
    pub value: f64,
    pub physical_op: String,
    pub logical_op: String,
    pub estimated_rows: f64,
    pub children: Vec<TreemapNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatmapCell {
    pub row: String,
    pub col: String,
    pub seeks: u64,
    pub scans: u64,
    pub lookups: u64,
    pub updates: u64,
    pub score: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeNode {
    pub schema: String,
    pub table: String,
    pub index: String,
    pub row_count: u64,
    pub reserved_kb: u64,
    pub used_kb: u64,
    pub data_kb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeverityBucket {
    pub line: u32,
    pub critical: u32,
    pub error: u32,
    pub warning: u32,
    pub info: u32,
}

pub fn plan_treemap(plan: &PlanNode) -> Vec<TreemapNode> {
    vec![to_node(plan)]
}

fn to_node(p: &PlanNode) -> TreemapNode {
    let mut self_cost = p.estimated_total_subtree_cost;
    for c in &p.children { self_cost -= c.estimated_total_subtree_cost; }
    if self_cost < 0.0 { self_cost = 0.0; }
    let children: Vec<TreemapNode> = p.children.iter().map(to_node).collect();
    let value = if children.is_empty() { p.estimated_total_subtree_cost.max(self_cost) } else { self_cost };
    TreemapNode {
        name: format!("{} ({})", p.physical_op, p.node_id),
        value: value.max(0.0001),
        physical_op: p.physical_op.clone(),
        logical_op: p.logical_op.clone(),
        estimated_rows: p.estimated_rows,
        children,
    }
}

pub fn severity_timeline(src: &str, findings: &[Finding]) -> Vec<SeverityBucket> {
    let total_lines = src.lines().count().max(1) as u32;
    let mut buckets: Vec<SeverityBucket> = (1..=total_lines)
        .map(|line| SeverityBucket { line, critical: 0, error: 0, warning: 0, info: 0 })
        .collect();
    for f in findings {
        let line = f.location.as_ref().map(|l| l.line).unwrap_or(1);
        if line == 0 || line as usize > buckets.len() { continue; }
        let b = &mut buckets[(line - 1) as usize];
        match f.severity {
            Severity::Critical => b.critical += 1,
            Severity::Error => b.error += 1,
            Severity::Warning => b.warning += 1,
            Severity::Info => b.info += 1,
        }
    }
    buckets
}
