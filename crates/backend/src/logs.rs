//! Durable log endpoints (AI interactions + analysis history).
//!
//! Storage shares the same SQLite file the sentinel daemon writes to
//! (`~/.dbopt/sentinel.db`), so logs survive backend restarts and browser
//! cache clears. The schema lives in sentinel/migrations/0003_logs.sql.

use axum::{extract::Query, http::StatusCode, response::IntoResponse, Json};
use chrono::{DateTime, Utc};
use sentinel::{
    storage::{AiInteractionRow, AnalysisFindingRow, AnalysisRunRow, Storage},
    SentinelConfig,
};
use serde::{Deserialize, Serialize};

fn open_storage() -> Option<Storage> {
    let path = SentinelConfig::default_db_path();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    Storage::open(&path).ok()
}

// ---------- AI log -----------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AiLogPostReq {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    pub response: String,
    pub status: String,
    pub error_message: Option<String>,
    pub latency_ms: Option<i64>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct AiLogEntry {
    pub id: String,
    pub occurred_at: String,
    pub provider: String,
    pub model: String,
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    pub response: String,
    pub status: String,
    pub error_message: Option<String>,
    pub latency_ms: Option<i64>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
}

impl From<AiInteractionRow> for AiLogEntry {
    fn from(r: AiInteractionRow) -> Self {
        Self {
            id: r.id,
            occurred_at: r.occurred_at.to_rfc3339(),
            provider: r.provider,
            model: r.model,
            system_prompt: r.system_prompt,
            user_prompt: r.user_prompt,
            response: r.response,
            status: r.status,
            error_message: r.error_message,
            latency_ms: r.latency_ms,
            tokens_in: r.tokens_in,
            tokens_out: r.tokens_out,
        }
    }
}

pub async fn post_ai_log(Json(req): Json<AiLogPostReq>) -> impl IntoResponse {
    let Some(storage) = open_storage() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":"storage unavailable"}))).into_response();
    };
    let row = AiInteractionRow {
        id: req.id,
        occurred_at: req.occurred_at.unwrap_or_else(Utc::now),
        provider: req.provider,
        model: req.model,
        system_prompt: req.system_prompt,
        user_prompt: req.user_prompt,
        response: req.response,
        status: req.status,
        error_message: req.error_message,
        latency_ms: req.latency_ms,
        tokens_in: req.tokens_in,
        tokens_out: req.tokens_out,
    };
    match storage.upsert_ai_interaction(&row) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"ok": false, "error": e.to_string()}))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    pub limit: Option<i64>,
}

pub async fn get_ai_log(Query(q): Query<LimitQuery>) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let Some(storage) = open_storage() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"entries": []}))).into_response();
    };
    let entries: Vec<AiLogEntry> = storage
        .list_ai_interactions(limit)
        .unwrap_or_default()
        .into_iter()
        .map(Into::into)
        .collect();
    (StatusCode::OK, Json(serde_json::json!({"entries": entries}))).into_response()
}

pub async fn delete_ai_log() -> impl IntoResponse {
    let Some(storage) = open_storage() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"ok":false}))).into_response();
    };
    let n = storage.clear_ai_interactions().unwrap_or(0);
    (StatusCode::OK, Json(serde_json::json!({"ok": true, "deleted": n}))).into_response()
}

// ---------- Analysis history -------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AnalysisRunPostReq {
    pub id: String,
    pub server_name: Option<String>,
    pub database_name: Option<String>,
    pub mode: String,
    pub sql_hash: Option<String>,
    pub sql_preview: Option<String>,
    pub server_version: Option<i64>,
    pub findings_total: i64,
    pub findings_critical: i64,
    pub findings_error: i64,
    pub findings_warning: i64,
    pub findings_info: i64,
    pub plan_attached: bool,
    pub plan_subtree_cost: Option<f64>,
    pub plan_op_count: Option<i64>,
    pub duration_ms: Option<i64>,
    pub findings: Vec<FindingPostReq>,
}

#[derive(Debug, Deserialize)]
pub struct FindingPostReq {
    pub rule_id: String,
    pub severity: String,
    pub line_no: Option<i64>,
    pub col_no: Option<i64>,
    pub message: String,
    pub recommendation: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AnalysisRunEntry {
    pub id: String,
    pub occurred_at: String,
    pub server_name: Option<String>,
    pub database_name: Option<String>,
    pub mode: String,
    pub sql_hash: Option<String>,
    pub sql_preview: Option<String>,
    pub server_version: Option<i64>,
    pub findings_total: i64,
    pub findings_critical: i64,
    pub findings_error: i64,
    pub findings_warning: i64,
    pub findings_info: i64,
    pub plan_attached: bool,
    pub plan_subtree_cost: Option<f64>,
    pub plan_op_count: Option<i64>,
    pub duration_ms: Option<i64>,
}

impl From<AnalysisRunRow> for AnalysisRunEntry {
    fn from(r: AnalysisRunRow) -> Self {
        Self {
            id: r.id,
            occurred_at: r.occurred_at.to_rfc3339(),
            server_name: r.server_name,
            database_name: r.database_name,
            mode: r.mode,
            sql_hash: r.sql_hash,
            sql_preview: r.sql_preview,
            server_version: r.server_version,
            findings_total: r.findings_total,
            findings_critical: r.findings_critical,
            findings_error: r.findings_error,
            findings_warning: r.findings_warning,
            findings_info: r.findings_info,
            plan_attached: r.plan_attached,
            plan_subtree_cost: r.plan_subtree_cost,
            plan_op_count: r.plan_op_count,
            duration_ms: r.duration_ms,
        }
    }
}

pub async fn post_analysis_run(Json(req): Json<AnalysisRunPostReq>) -> impl IntoResponse {
    let Some(storage) = open_storage() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"ok":false}))).into_response();
    };
    let run = AnalysisRunRow {
        id: req.id.clone(),
        occurred_at: Utc::now(),
        server_name: req.server_name,
        database_name: req.database_name,
        mode: req.mode,
        sql_hash: req.sql_hash,
        sql_preview: req.sql_preview,
        server_version: req.server_version,
        findings_total: req.findings_total,
        findings_critical: req.findings_critical,
        findings_error: req.findings_error,
        findings_warning: req.findings_warning,
        findings_info: req.findings_info,
        plan_attached: req.plan_attached,
        plan_subtree_cost: req.plan_subtree_cost,
        plan_op_count: req.plan_op_count,
        duration_ms: req.duration_ms,
    };
    let findings: Vec<AnalysisFindingRow> = req
        .findings
        .into_iter()
        .map(|f| AnalysisFindingRow {
            run_id: req.id.clone(),
            rule_id: f.rule_id,
            severity: f.severity,
            line_no: f.line_no,
            col_no: f.col_no,
            message: f.message,
            recommendation: f.recommendation,
        })
        .collect();
    match storage.insert_analysis_run(&run, &findings) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"ok":false, "error": e.to_string()}))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct AnalysisListQuery {
    pub limit: Option<i64>,
    pub server: Option<String>,
    pub database: Option<String>,
}

pub async fn get_analysis_runs(Query(q): Query<AnalysisListQuery>) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(200).clamp(1, 5000);
    let Some(storage) = open_storage() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"runs": []}))).into_response();
    };
    let runs: Vec<AnalysisRunEntry> = storage
        .list_analysis_runs(q.server.as_deref(), q.database.as_deref(), limit)
        .unwrap_or_default()
        .into_iter()
        .map(Into::into)
        .collect();
    (StatusCode::OK, Json(serde_json::json!({"runs": runs}))).into_response()
}

#[derive(Debug, Deserialize)]
pub struct RunIdQuery {
    pub id: String,
}

pub async fn get_analysis_findings(Query(q): Query<RunIdQuery>) -> impl IntoResponse {
    let Some(storage) = open_storage() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"findings": []}))).into_response();
    };
    let findings: Vec<_> = storage
        .list_findings_for_run(&q.id)
        .unwrap_or_default()
        .into_iter()
        .map(|f| {
            serde_json::json!({
                "rule": f.rule_id,
                "severity": f.severity,
                "line": f.line_no,
                "col": f.col_no,
                "message": f.message,
                "recommendation": f.recommendation,
            })
        })
        .collect();
    (StatusCode::OK, Json(serde_json::json!({"findings": findings}))).into_response()
}
