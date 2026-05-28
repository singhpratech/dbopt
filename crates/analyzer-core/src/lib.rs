pub mod findings;
pub mod tokens;
pub mod rules;
pub mod plan_xml;
pub mod dmv;
pub mod report;

#[cfg(test)]
mod tests;

pub use findings::{Finding, Severity, RuleId, Location};
pub use report::{AnalysisReport, ChartData};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnalyzeInput {
    pub sql: Option<String>,
    pub plan_xml: Option<String>,
    pub dmv_bundle: Option<dmv::DmvBundle>,
    pub server_version: Option<u16>,
}

pub fn analyze(input: &AnalyzeInput) -> AnalysisReport {
    let mut report = AnalysisReport::default();

    if let Some(sql) = input.sql.as_deref() {
        let tokens = tokens::tokenize(sql);
        report.findings.extend(rules::run_all(sql, &tokens, input.server_version));
        report.charts.severity_timeline = report::severity_timeline(sql, &report.findings);
    }

    if let Some(xml) = input.plan_xml.as_deref() {
        match plan_xml::parse(xml) {
            Ok(plan) => {
                report.charts.plan_treemap = report::plan_treemap(&plan);
                report.findings.extend(plan_xml::derive_findings(&plan));
            }
            Err(e) => report.findings.push(Finding {
                rule: RuleId("plan_xml.parse_error".into()),
                severity: Severity::Error,
                message: format!("Could not parse execution plan XML: {e}"),
                location: None,
                recommendation: None,
            }),
        }
    }

    if let Some(bundle) = input.dmv_bundle.as_ref() {
        let advice = dmv::analyze(bundle);
        report.charts.index_heatmap = advice.index_heatmap;
        report.charts.size_treemap = advice.size_treemap;
        report.findings.extend(advice.findings);
    }

    report.findings.sort_by_key(|f| (f.severity.rank(), f.location.as_ref().map(|l| l.start).unwrap_or(0)));
    report
}
