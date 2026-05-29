use axum::{
    extract::{Json, Path},
    http::StatusCode,
    response::{sse::{Event, Sse}, IntoResponse},
    routing::{get, post},
    Router,
};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

use crate::{health, logs, ollama, providers, scan, sentinel_api, sqlserver};

pub fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/connect", post(connect))
        .route("/databases", post(databases))
        .route("/advise", post(advise))
        .route("/health/db", post(health_db))
        .route("/health/issue/detail", post(crate::health::enrichment::issue_detail))
        .route("/dmv", post(dmv))
        .route("/explain", post(explain))
        .route("/llm/models", get(llm_models))
        .route("/llm/chat", post(llm_chat))
        .route("/llm/cloud/:provider", post(llm_cloud))
        .route("/llm/cloud/:provider/models", post(llm_cloud_models))
        .route("/llm/cloud/:provider/test", post(llm_cloud_test))
        .route("/analyze", post(analyze))
        .route("/sentinel/start", post(sentinel_api::start))
        .route("/sentinel/stop", post(sentinel_api::stop))
        .route("/sentinel/status", get(sentinel_api::status))
        .route("/sentinel/report", get(sentinel_api::report_json))
        .route("/sentinel/report.md", get(sentinel_api::report_markdown))
        .route("/sentinel/report.html", get(sentinel_api::report_html))
        // ---- durable logs (AI + analysis history) ----------------------
        .route("/logs/ai",       get(logs::get_ai_log).post(logs::post_ai_log).delete(logs::delete_ai_log))
        .route("/logs/analysis", get(logs::get_analysis_runs).post(logs::post_analysis_run))
        .route("/logs/analysis/findings", get(logs::get_analysis_findings))
        .route("/scan/database", post(scan::scan_database))
}

async fn health() -> &'static str { "ok" }

#[derive(Debug, Deserialize)]
pub struct ConnectReq {
    pub server: String,
    pub database: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub trust_cert: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ConnectResp { pub ok: bool, pub server_version: Option<String>, pub error: Option<String> }

async fn connect(Json(req): Json<ConnectReq>) -> impl IntoResponse {
    match sqlserver::ping(&req).await {
        Ok(v) => (StatusCode::OK, Json(ConnectResp { ok: true, server_version: Some(v), error: None })),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(ConnectResp { ok: false, server_version: None, error: Some(e.to_string()) })),
    }
}

async fn dmv(Json(req): Json<ConnectReq>) -> impl IntoResponse {
    match sqlserver::pull_dmv_bundle(&req).await {
        Ok(bundle) => (StatusCode::OK, Json(serde_json::to_value(&bundle).unwrap())).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

async fn databases(Json(req): Json<ConnectReq>) -> impl IntoResponse {
    match sqlserver::list_databases(&req).await {
        Ok(names) => (StatusCode::OK, Json(serde_json::json!({ "databases": names }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// Connect, pull the live DMV bundle, and return ranked prescriptive
/// recommendations (the advisor) plus the bundle's findings/charts in one call.
async fn advise(Json(req): Json<ConnectReq>) -> impl IntoResponse {
    match sqlserver::pull_dmv_bundle(&req).await {
        Ok(bundle) => {
            let recommendations = analyzer_core::dmv::advise(&bundle);
            let advice = analyzer_core::dmv::analyze(&bundle);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "recommendations": recommendations,
                    "findings": advice.findings,
                    "index_heatmap": advice.index_heatmap,
                    "size_treemap": advice.size_treemap,
                })),
            )
                .into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/health/db body: the same connection payload as `/advise`, plus an
/// optional `engine` selector (defaults to `"sqlserver"`).
#[derive(Debug, Deserialize)]
pub struct HealthReq {
    #[serde(flatten)]
    pub conn: ConnectReq,
    pub engine: Option<String>,
}

/// Aggregated, engine-neutral database health front-door. Dispatches to the
/// per-engine `HealthProvider`; unknown engines → 400, unimplemented → 501,
/// provider failures (e.g. connection) → 502.
async fn health_db(Json(req): Json<HealthReq>) -> impl IntoResponse {
    let engine = req.engine.as_deref().unwrap_or("sqlserver");
    match health::run(engine, &req.conn).await {
        Ok(report) => (StatusCode::OK, Json(serde_json::to_value(&report).unwrap())).into_response(),
        Err((code, msg)) => (code, Json(serde_json::json!({ "error": msg }))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct ExplainReq {
    #[serde(flatten)]
    pub conn: ConnectReq,
    pub sql: String,
}

async fn explain(Json(req): Json<ExplainReq>) -> impl IntoResponse {
    match sqlserver::estimated_plan(&req.conn, &req.sql).await {
        Ok(plan) => (StatusCode::OK, Json(serde_json::json!({ "plan_xml": plan }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn llm_models() -> impl IntoResponse {
    match ollama::list_models().await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct ChatReq {
    pub model: Option<String>,
    pub messages: Vec<ollama::Message>,
    pub options: Option<serde_json::Value>,
}

async fn llm_chat(Json(req): Json<ChatReq>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let model = req.model.unwrap_or_else(|| "gemma4:e4b".into());
    let stream = ollama::stream_chat(model, req.messages, req.options);
    Sse::new(stream).keep_alive(Default::default())
}

#[derive(Debug, Deserialize)]
pub struct CloudChatReq {
    pub config: serde_json::Value,
    pub messages: Vec<ollama::Message>,
}

async fn llm_cloud(
    Path(provider): Path<String>,
    Json(req): Json<CloudChatReq>,
) -> axum::response::Response {
    use providers::{anthropic, bedrock, openai_compat};
    fn bad(e: impl ToString) -> axum::response::Response {
        (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": e.to_string()}))).into_response()
    }
    match provider.as_str() {
        "anthropic" => match serde_json::from_value::<anthropic::Config>(req.config) {
            Ok(cfg) => Sse::new(anthropic::stream_chat(cfg, req.messages)).keep_alive(Default::default()).into_response(),
            Err(e) => bad(e),
        },
        "openai" | "openrouter" | "azure" => match serde_json::from_value::<openai_compat::Config>(req.config) {
            Ok(mut cfg) => {
                cfg.provider = provider.clone();
                Sse::new(openai_compat::stream_chat(cfg, req.messages)).keep_alive(Default::default()).into_response()
            }
            Err(e) => bad(e),
        },
        "bedrock" => match serde_json::from_value::<bedrock::Config>(req.config) {
            Ok(cfg) => Sse::new(bedrock::stream_chat(cfg, req.messages)).keep_alive(Default::default()).into_response(),
            Err(e) => bad(e),
        },
        other => bad(format!("unknown provider: {other}")),
    }
}

#[derive(Debug, Deserialize)]
pub struct DiscoverReq {
    pub config: serde_json::Value,
}

/// List a cloud provider's available models (proxied to dodge browser CORS).
async fn llm_cloud_models(Path(provider): Path<String>, Json(req): Json<DiscoverReq>) -> impl IntoResponse {
    match providers::discover::list_models(&provider, &req.config).await {
        Ok(models) => (StatusCode::OK, Json(serde_json::json!({ "models": models }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e }))).into_response(),
    }
}

/// Validate a cloud provider API key (and, for OpenRouter, report credits).
async fn llm_cloud_test(Path(provider): Path<String>, Json(req): Json<DiscoverReq>) -> impl IntoResponse {
    match providers::discover::test_key(&provider, &req.config).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e }))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct AnalyzeReq {
    pub sql: Option<String>,
    pub plan_xml: Option<String>,
    pub dmv_bundle: Option<serde_json::Value>,
    pub server_version: Option<u16>,
}

async fn analyze(Json(req): Json<AnalyzeReq>) -> impl IntoResponse {
    let input = analyzer_core::AnalyzeInput {
        sql: req.sql,
        plan_xml: req.plan_xml,
        dmv_bundle: req.dmv_bundle.and_then(|v| serde_json::from_value(v).ok()),
        server_version: req.server_version,
        engine: None, // SQL Server (v0.x default)
    };
    let report = analyzer_core::analyze(&input);
    (StatusCode::OK, Json(report))
}
