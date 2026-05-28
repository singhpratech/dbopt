//! Database-level analysis endpoint.
//!
//! Pulls every user-defined programmable object from `sys.sql_modules`,
//! runs the static analyzer against each body, and returns a per-object
//! breakdown alongside an aggregate rule-incidence summary. The cost is
//! bounded by the number of modules (typically tens to a few hundred for
//! application databases); each analyze pass is pure CPU on the backend.

use axum::{extract::Json, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{routes::ConnectReq, sqlserver};

#[derive(Debug, Deserialize)]
pub struct ScanReq {
    #[serde(flatten)]
    pub conn: ConnectReq,
    #[serde(default = "default_version")]
    pub server_version: u16,
}

fn default_version() -> u16 { 2025 }

#[derive(Debug, Serialize)]
pub struct ScanObjectResult {
    pub schema_name: String,
    pub object_name: String,
    pub object_type: String,
    pub body_length: usize,
    pub findings_total: usize,
    pub findings_critical: usize,
    pub findings_error: usize,
    pub findings_warning: usize,
    pub findings_info: usize,
    pub top_rules: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ScanResult {
    pub server: String,
    pub database: Option<String>,
    pub objects_scanned: usize,
    pub findings_total: usize,
    pub findings_critical: usize,
    pub findings_error: usize,
    pub findings_warning: usize,
    pub findings_info: usize,
    pub rule_incidence: Vec<(String, usize)>,
    pub objects: Vec<ScanObjectResult>,
    pub duration_ms: u64,
}

pub async fn scan_database(Json(req): Json<ScanReq>) -> impl IntoResponse {
    let started = std::time::Instant::now();
    let modules = match sqlserver::enumerate_modules(&req.conn).await {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let mut total_c = 0usize;
    let mut total_e = 0usize;
    let mut total_w = 0usize;
    let mut total_i = 0usize;
    let mut rule_hits: HashMap<String, usize> = HashMap::new();
    let mut objects: Vec<ScanObjectResult> = Vec::with_capacity(modules.len());

    for m in modules {
        let input = analyzer_core::AnalyzeInput {
            sql: Some(m.body.clone()),
            plan_xml: None,
            dmv_bundle: None,
            server_version: Some(req.server_version),
        };
        let report = analyzer_core::analyze(&input);
        let mut c = 0usize;
        let mut e = 0usize;
        let mut w = 0usize;
        let mut i = 0usize;
        let mut local_rules: HashMap<String, usize> = HashMap::new();
        for f in &report.findings {
            match f.severity {
                analyzer_core::findings::Severity::Critical => c += 1,
                analyzer_core::findings::Severity::Error => e += 1,
                analyzer_core::findings::Severity::Warning => w += 1,
                analyzer_core::findings::Severity::Info => i += 1,
            }
            *local_rules.entry(f.rule.0.clone()).or_insert(0) += 1;
            *rule_hits.entry(f.rule.0.clone()).or_insert(0) += 1;
        }
        let mut sorted_local: Vec<(String, usize)> = local_rules.into_iter().collect();
        sorted_local.sort_by(|a, b| b.1.cmp(&a.1));
        let top_rules = sorted_local.into_iter().take(5).map(|(k, _)| k).collect();

        total_c += c; total_e += e; total_w += w; total_i += i;
        objects.push(ScanObjectResult {
            schema_name: m.schema_name,
            object_name: m.object_name,
            object_type: m.object_type,
            body_length: m.body.len(),
            findings_total: c + e + w + i,
            findings_critical: c,
            findings_error: e,
            findings_warning: w,
            findings_info: i,
            top_rules,
        });
    }

    // Sort objects: most-painful first.
    objects.sort_by(|a, b| {
        b.findings_critical.cmp(&a.findings_critical)
            .then(b.findings_error.cmp(&a.findings_error))
            .then(b.findings_warning.cmp(&a.findings_warning))
            .then(b.findings_total.cmp(&a.findings_total))
    });

    let mut rule_incidence: Vec<(String, usize)> = rule_hits.into_iter().collect();
    rule_incidence.sort_by(|a, b| b.1.cmp(&a.1));

    let result = ScanResult {
        server: req.conn.server.clone(),
        database: req.conn.database.clone(),
        objects_scanned: objects.len(),
        findings_total: total_c + total_e + total_w + total_i,
        findings_critical: total_c,
        findings_error: total_e,
        findings_warning: total_w,
        findings_info: total_i,
        rule_incidence,
        objects,
        duration_ms: started.elapsed().as_millis() as u64,
    };
    (StatusCode::OK, Json(result)).into_response()
}
