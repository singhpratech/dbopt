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
        .route("/capabilities", get(capabilities))
        .route("/version", get(version))
        .route("/shutdown", post(shutdown))
        .route("/connect", post(connect))
        .route("/databases", post(databases))
        .route("/advise", post(advise))
        .route("/health/db", post(health_db))
        .route("/health/issue/detail", post(crate::health::enrichment::issue_detail))
        .route("/dmv", post(dmv))
        .route("/monitor/live", post(monitor_live))
        .route("/monitor/vitals", post(monitor_vitals))
        .route("/explain", post(explain))
        .route("/plan/actual", post(plan_actual))
        .route("/validate", post(validate))
        .route("/qstore/status", post(qstore_status))
        .route("/qstore/capture", post(qstore_capture))
        .route("/qstore/top", post(qstore_top))
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
        // ---- threshold alerting (fired-alert feed + config) ------------
        .route("/alerts", get(sentinel_api::alerts))
        .route("/alerts/config", get(sentinel_api::get_alert_config).post(sentinel_api::set_alert_config))
        // ---- durable logs (AI + analysis history) ----------------------
        .route("/logs/ai",       get(logs::get_ai_log).post(logs::post_ai_log).delete(logs::delete_ai_log))
        .route("/logs/analysis", get(logs::get_analysis_runs).post(logs::post_analysis_run))
        .route("/logs/analysis/findings", get(logs::get_analysis_findings))
        .route("/scan/database", post(scan::scan_database))
}

async fn health() -> &'static str { "ok" }

/// What THIS binary can actually do. The UI gates connection options on this so
/// it never offers a path the build can't honor.
///
/// Windows authentication has two flavors, with different build requirements:
///   - integrated (current user / trusted connection): the Windows release build
///     (winauth/SSPI) OR a Linux build made with `--features integrated-auth`
///     (Kerberos/GSSAPI).
///   - explicit Windows account (DOMAIN\user + password, NTLM): Windows + winauth
///     only.
async fn capabilities() -> impl IntoResponse {
    let integrated_auth = cfg!(windows) || cfg!(all(unix, feature = "integrated-auth"));
    let windows_account_auth = cfg!(windows);
    Json(serde_json::json!({
        // Kept under its original key for UI back-compat: "can do Windows
        // integrated (current-user) auth on this build".
        "integrated_auth": integrated_auth,
        // Can authenticate with an explicit Windows account + password (NTLM).
        "windows_account_auth": windows_account_auth,
        // AWS Bedrock is behind an opt-in build feature (heavy AWS SDK). Shipped
        // release binaries don't include it, so the UI must gate it honestly
        // rather than offer a provider that errors the moment it's used.
        "bedrock": cfg!(feature = "bedrock"),
        "platform": std::env::consts::OS,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// The running binary's identity — used by the in-app "Check for updates" flow
/// to compare against the latest GitHub release. Purely local: it reads compile-
/// time constants only and makes NO network call. The update check itself is a
/// browser→GitHub request, fired once on launch by default (opt-out) and
/// whenever the user clicks "Check for updates" — see docs/DATA-HANDLING.md.
async fn version() -> impl IntoResponse {
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,    // "windows" | "macos" | "linux"
        "arch": std::env::consts::ARCH,       // "x86_64" | "aarch64" | …
    }))
}

/// Gracefully stop the server so an installer isn't blocked by the running
/// binary — the "Quit dbopt" step of the in-app update flow. On Windows the MSI
/// does an in-place major upgrade, which needs `dbopt.exe` not to be locked;
/// macOS/Linux just want the old process gone before the new files land.
///
/// The server binds 127.0.0.1 only, so this is reachable solely from the local
/// machine. We flush the 200 response first, then exit the whole process (which
/// also stops any in-process Sentinel poller) a beat later.
async fn shutdown() -> impl IntoResponse {
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        std::process::exit(0);
    });
    Json(serde_json::json!({ "stopping": true }))
}

#[derive(Debug, Deserialize)]
pub struct ConnectReq {
    pub server: String,
    pub database: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub trust_cert: Option<bool>,
    /// `"sql"` | `"integrated"` | `"windows"`. Absent ⇒ inferred from whether a
    /// username is present (see `sqlserver::apply_auth`).
    #[serde(default)]
    pub auth_mode: Option<String>,
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
        // `Json` serializes on the fly and returns a 500 on the (practically
        // impossible) serialization failure — never a handler panic.
        Ok(bundle) => (StatusCode::OK, Json(bundle)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

/// POST /api/monitor/live — one real-time snapshot of server vitals (CPU,
/// waits, batch/sec, IO, live sessions). The UI polls this on an interval and
/// renders scrolling line charts (Activity-Monitor style). DMV-only; no rows.
async fn monitor_live(Json(req): Json<ConnectReq>) -> impl IntoResponse {
    match sqlserver::pull_live_metrics(&req).await {
        Ok(m) => (StatusCode::OK, Json(m)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/monitor/vitals — the most-recent DEEP-VITALS sample of each
/// surface (CPU pressure, memory headroom, per-file I/O latency, tempdb
/// allocation contention, plan-cache health) that the background monitor has
/// persisted for the connected server.
///
/// Read-only: opens the sentinel SQLite store and reads it back — it never
/// touches the live server. If the store doesn't exist yet, or the server has
/// never been monitored, it returns 200 with `has_data: false` (an honest empty
/// state, NOT an error) so the UI can prompt the user to start the monitor.
async fn monitor_vitals(Json(req): Json<ConnectReq>) -> impl IntoResponse {
    (StatusCode::OK, Json(sentinel_api::deep_vitals(&req.server))).into_response()
}

async fn databases(Json(req): Json<ConnectReq>) -> impl IntoResponse {
    match sqlserver::list_databases(&req).await {
        Ok(dbs) => (StatusCode::OK, Json(serde_json::json!({ "databases": dbs }))).into_response(),
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
            // Honest transparency: how many tables the live Query-Store workload
            // grounding actually covered, and the capture window it observed.
            // `null` window when no table matched (Query Store off/empty) so the
            // UI can say "workload grounding unavailable" instead of implying 0h.
            let workload_window_hours = bundle.workload.first().map(|w| w.window_hours);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "recommendations": recommendations,
                    "findings": advice.findings,
                    "index_heatmap": advice.index_heatmap,
                    "size_treemap": advice.size_treemap,
                    "workload_tables": bundle.workload.len(),
                    "workload_window_hours": workload_window_hours,
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
        Ok(report) => (StatusCode::OK, Json(report)).into_response(),
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

/// POST /api/plan/actual — execute the batch with the ACTUAL plan captured,
/// inside a transaction that is ALWAYS rolled back (DML leaves no trace).
/// Destructive / DDL / EXEC batches are refused server-side. Returns { plan_xml }.
async fn plan_actual(Json(req): Json<ExplainReq>) -> impl IntoResponse {
    match sqlserver::actual_plan(&req.conn, &req.sql).await {
        Ok(plan) => (StatusCode::OK, Json(serde_json::json!({ "plan_xml": plan }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/validate — SSMS-style "Parse" of a T-SQL batch against the real
/// engine (SET PARSEONLY ON). Returns `{ ok, diagnostics: [{number,line,message}] }`.
/// `ok:true` with empty diagnostics = clean parse. A connection/transport
/// failure (not a syntax verdict) returns 502.
async fn validate(Json(req): Json<ExplainReq>) -> impl IntoResponse {
    match sqlserver::parse_check(&req.conn, &req.sql).await {
        Ok(diags) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": diags.is_empty(), "diagnostics": diags })),
        )
            .into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/qstore/status — Query Store config for the connected database.
async fn qstore_status(Json(req): Json<ConnectReq>) -> impl IntoResponse {
    match sqlserver::query_store_status(&req).await {
        Ok(s) => (StatusCode::OK, Json(s)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/qstore/top — the connected database's top long-running queries from
/// Query Store, ranked by average duration. READ-ONLY telemetry from the
/// sys.query_store_* catalog views: no query execution, no user-table rows read.
#[derive(Debug, Deserialize)]
pub struct QStoreTopReq {
    #[serde(flatten)]
    pub conn: ConnectReq,
    /// How many queries to return (clamped 1..=200 server-side; default 20).
    pub limit: Option<u32>,
}

async fn qstore_top(Json(req): Json<QStoreTopReq>) -> impl IntoResponse {
    match sqlserver::query_store_top_queries(&req.conn, req.limit.unwrap_or(20)).await {
        Ok(rows) => (StatusCode::OK, Json(serde_json::json!({ "queries": rows }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct QStoreCaptureReq {
    #[serde(flatten)]
    pub conn: ConnectReq,
    /// AUTO | ALL | NONE — validated server-side against an allowlist.
    pub mode: String,
}

/// POST /api/qstore/capture — set the connected DB's Query Store capture mode.
/// This runs DDL; the UI must preview the statement and get explicit user
/// confirmation first (Safe-Apply). Mode is allowlisted in `set_query_store_capture`.
async fn qstore_capture(Json(req): Json<QStoreCaptureReq>) -> impl IntoResponse {
    match sqlserver::set_query_store_capture(&req.conn, &req.mode).await {
        Ok(msg) => (StatusCode::OK, Json(serde_json::json!({ "ok": true, "message": msg }))).into_response(),
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

/// Client mistakes -> 400, upstream/network failures -> 502. Messages are
/// already sanitized by the discover layer (never an upstream body).
fn discover_err(e: providers::discover::DiscoverError) -> axum::response::Response {
    use providers::discover::DiscoverError::*;
    let (code, msg) = match e {
        BadRequest(m) => (StatusCode::BAD_REQUEST, m),
        Upstream(m) => (StatusCode::BAD_GATEWAY, m),
    };
    (code, Json(serde_json::json!({ "error": msg }))).into_response()
}

/// List a cloud provider's available models (proxied to dodge browser CORS).
async fn llm_cloud_models(Path(provider): Path<String>, Json(req): Json<DiscoverReq>) -> axum::response::Response {
    match providers::discover::list_models(&provider, &req.config).await {
        Ok(models) => (StatusCode::OK, Json(serde_json::json!({ "models": models }))).into_response(),
        Err(e) => discover_err(e),
    }
}

/// Validate a cloud provider API key (and, for OpenRouter, report credits).
async fn llm_cloud_test(Path(provider): Path<String>, Json(req): Json<DiscoverReq>) -> axum::response::Response {
    match providers::discover::test_key(&provider, &req.config).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(e) => discover_err(e),
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
