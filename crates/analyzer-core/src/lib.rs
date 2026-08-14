pub mod findings;
pub mod tokens;
pub mod rules;
pub mod plan_xml;
pub mod dmv;
pub mod advisor_workload;
pub mod report;

#[cfg(test)]
mod tests;

pub use findings::{Finding, Severity, RuleId, Location, ObjectRef};
pub use report::{AnalysisReport, ChartData};

use serde::{Deserialize, Serialize};

/// Target database engine. SQL Server is the only implemented engine in v0.x;
/// Postgres/MySQL are placeholders for the v1.0 multi-engine work. Rules declare
/// which engines they apply to (see `rules::Rule`), and the analyzer skips rules
/// that don't apply to the requested engine. Defaults to SQL Server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    #[default]
    SqlServer,
    Postgres,
    MySql,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnalyzeInput {
    pub sql: Option<String>,
    pub plan_xml: Option<String>,
    pub dmv_bundle: Option<dmv::DmvBundle>,
    pub server_version: Option<u16>,
    /// Target engine; `None` is treated as SQL Server (the v0.x default).
    #[serde(default)]
    pub engine: Option<Engine>,
}

pub fn analyze(input: &AnalyzeInput) -> AnalysisReport {
    let mut report = AnalysisReport::default();
    let engine = input.engine.unwrap_or_default();

    if let Some(sql) = input.sql.as_deref() {
        let tokens = tokens::tokenize(sql);
        report.findings.extend(rules::run_all(sql, &tokens, input.server_version, engine));
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

                object: None,
            }),
        }
    }

    if let Some(bundle) = input.dmv_bundle.as_ref() {
        let advice = dmv::analyze(bundle);
        report.charts.index_heatmap = advice.index_heatmap;
        report.charts.size_treemap = advice.size_treemap;
        report.findings.extend(advice.findings);
        // Prescriptive, ranked remediation with copy-paste T-SQL.
        report.recommendations = dmv::advise(bundle);
    }

    report.findings.sort_by_key(|f| (f.severity.rank(), f.location.as_ref().map(|l| l.start).unwrap_or(0)));
    report
}
