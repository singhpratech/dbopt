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

/// A `Json<T>` whose rejection is an `{"error": …}` body like every other API
/// failure, instead of axum's default plain-text
/// `Failed to deserialize the JSON body into the target type: missing field \`server\``.
///
/// Two reasons: the UI parses `error` and rendered that raw string at the user,
/// and the default message narrates internal field names back to any caller.
pub struct ApiJson<T>(pub T);

#[axum::async_trait]
impl<T, S> axum::extract::FromRequest<S> for ApiJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request(
        req: axum::extract::Request,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(ApiJson(value)),
            Err(rejection) => {
                tracing::debug!(target: "backend::api", "request body rejected: {rejection}");
                Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "the request body was not in the expected format"
                    })),
                )
                    .into_response())
            }
        }
    }
}


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

async fn connect(ApiJson(req): ApiJson<ConnectReq>) -> impl IntoResponse {
    match sqlserver::ping(&req).await {
        Ok(v) => (StatusCode::OK, Json(ConnectResp { ok: true, server_version: Some(v), error: None })),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(ConnectResp { ok: false, server_version: None, error: Some(e.to_string()) })),
    }
}

/// A DMV scan that ran in a system database almost certainly ran there by
/// accident: a server-level connection with no database selected resolves to
/// the login's default (normally `master`), where `is_ms_shipped = 0` filters
/// out every user object. The result is an empty bundle that looks like a clean
/// bill of health. Returns the sentence to show the user, or `None` when the
/// scope is fine.
fn scope_warning(bundle: &analyzer_core::dmv::DmvBundle) -> Option<String> {
    let db = bundle.scanned_database.trim();
    if db.is_empty() {
        return None;
    }
    let is_system = sqlserver::SYSTEM_DATABASES
        .iter()
        .any(|s| s.eq_ignore_ascii_case(db));
    if is_system && bundle.indexes.is_empty() && bundle.index_usage.is_empty() {
        return Some(format!(
            "This scan ran in [{db}], a system database, and found no user tables — so \"no findings\" here does NOT mean your database is clean. Select the database you want to analyze and scan again."
        ));
    }
    None
}

async fn dmv(ApiJson(req): ApiJson<ConnectReq>) -> impl IntoResponse {
    match sqlserver::pull_dmv_bundle(&req).await {
        // `Json` serializes on the fly and returns a 500 on the (practically
        // impossible) serialization failure — never a handler panic.
        Ok(bundle) => {
            let warning = scope_warning(&bundle);
            let mut body = serde_json::to_value(&bundle).unwrap_or_else(|_| serde_json::json!({}));
            if let (Some(obj), Some(w)) = (body.as_object_mut(), warning) {
                obj.insert("scope_warning".into(), serde_json::Value::String(w));
            }
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": e.to_string()}))).into_response(),
    }
}

/// POST /api/monitor/live — one real-time snapshot of server vitals (CPU,
/// waits, batch/sec, IO, live sessions). The UI polls this on an interval and
/// renders scrolling line charts (Activity-Monitor style). DMV-only; no rows.
async fn monitor_live(ApiJson(req): ApiJson<ConnectReq>) -> impl IntoResponse {
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
async fn monitor_vitals(ApiJson(req): ApiJson<ConnectReq>) -> impl IntoResponse {
    (StatusCode::OK, Json(sentinel_api::deep_vitals(&req.server))).into_response()
}

async fn databases(ApiJson(req): ApiJson<ConnectReq>) -> impl IntoResponse {
    match sqlserver::list_databases(&req).await {
        Ok(dbs) => (StatusCode::OK, Json(serde_json::json!({ "databases": dbs }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// Connect, pull the live DMV bundle, and return ranked prescriptive
/// recommendations (the advisor) plus the bundle's findings/charts in one call.
async fn advise(ApiJson(req): ApiJson<ConnectReq>) -> impl IntoResponse {
    match sqlserver::pull_dmv_bundle(&req).await {
        Ok(mut bundle) => {
            // Monitor read-back: how persistently the missing-index DMV has
            // suggested each table across daily snapshots (empty if unmonitored).
            sentinel_api::attach_missing_index_history(&mut bundle, &req);
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
                    "scanned_database": bundle.scanned_database,
                    "scope_warning": scope_warning(&bundle),
                    // Lifetime of every usage counter behind the recs: the
                    // instance's last start (RFC 3339 UTC) and seconds since.
                    // `null` when the server would not tell us.
                    "counters_since": bundle.counters_since,
                    "counter_age_secs": bundle.counter_age_secs,
                    // Per-table "seen on N of M days" from the monitor's daily
                    // missing-index snapshots (`[]` when unmonitored).
                    "missing_index_history": bundle.missing_index_history,
                    "missing_index_history_days": sentinel_api::MISSING_INDEX_HISTORY_DAYS,
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
async fn health_db(ApiJson(req): ApiJson<HealthReq>) -> impl IntoResponse {
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
    /// ACTUAL-plan only: the caller has seen the cost estimate and still wants
    /// to run it. Absent/false makes an expensive batch return a 409 preflight
    /// instead of executing. Ignored by the estimated-plan path, which runs
    /// nothing.
    #[serde(default)]
    pub confirm_heavy: bool,
}

/// Estimated subtree cost above which an ACTUAL-plan run is treated as heavy
/// enough to confirm. SQL Server's cost unit is abstract, but the threshold is
/// calibrated against the same plans this tool already flags: a trivial lookup
/// is well under 1, and a scan-and-hash-join over millions of rows runs into
/// the hundreds. 100 sits above everyday interactive queries and below the
/// multi-minute stage builds a rollback-wrapped run would actually stall.
const HEAVY_PLAN_COST: f64 = 100.0;

async fn explain(ApiJson(req): ApiJson<ExplainReq>) -> impl IntoResponse {
    match sqlserver::estimated_plan(&req.conn, &req.sql).await {
        Ok(plan) => (StatusCode::OK, Json(serde_json::json!({ "plan_xml": plan }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/plan/actual — execute the batch with the ACTUAL plan captured,
/// inside a transaction that is ALWAYS rolled back (DML leaves no trace).
/// Destructive / DDL / EXEC batches are refused server-side. Returns { plan_xml }.
async fn plan_actual(ApiJson(req): ApiJson<ExplainReq>) -> impl IntoResponse {
    // Cost preflight. The batch is about to really execute — rolled back, but
    // the reads, the CPU and the tempdb are all real, and on a production box a
    // heavy stage is a multi-minute load event. Compiling the estimated plan
    // first costs nothing (SET SHOWPLAN_XML executes no statement) and tells us
    // what we are about to ask for, so the warning is a MEASURED estimate
    // rather than a generic "may take real time".
    //
    // A failure to preflight is never a reason to block: if we cannot compile
    // the estimate we fall through and let the actual run report the real error.
    if !req.confirm_heavy {
        if let Ok(plan_xml) = sqlserver::estimated_plan(&req.conn, &req.sql).await {
            if let Ok(plan) = analyzer_core::plan_xml::parse(&plan_xml) {
                let cost = plan.estimated_total_subtree_cost;
                let rows = plan.estimated_rows;
                if cost >= HEAVY_PLAN_COST {
                    return (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({
                            "error": format!(
                                "This batch is expensive to run: the optimizer estimates a cost of {cost:.0} over ~{rows:.0} rows. The actual-plan capture executes it for real (inside a transaction that is always rolled back), so on a busy server this is a genuine load event that may run for minutes. Re-send with confirm_heavy to run it anyway, or read the ESTIMATED plan, which executes nothing."
                            ),
                            "needs_confirmation": true,
                            "estimated_cost": cost,
                            "estimated_rows": rows,
                        })),
                    )
                        .into_response();
                }
            }
        }
    }
    match sqlserver::actual_plan(&req.conn, &req.sql).await {
        Ok(plan) => (StatusCode::OK, Json(serde_json::json!({ "plan_xml": plan }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// POST /api/validate — "Parse" of a T-SQL batch against the real engine
/// (SET PARSEONLY ON). Returns `{ ok, checked_by, diagnostics: [{number,line,message}] }`.
/// `ok:true` with empty diagnostics = clean parse. A connection/transport
/// failure (not a syntax verdict) returns 502.
///
/// One gap in PARSEONLY that this endpoint closes itself: the first statement
/// of a batch may be a bare procedure name (`sp_who` is shorthand for
/// `EXEC sp_who`), and PARSEONLY never binds names — so a misspelled first
/// keyword such as `SELCT 1` or `UPDAT t SET x = 1` is accepted by the server
/// as an implicit `EXEC SELCT 1`. Verified against SQL Server 2025: `SELCT 1`
/// parses clean, `SELCT * FROM t` does not. [`implicit_exec_diagnostics`]
/// flags that case so CHECK SYNTAX cannot pass a typo the engine would only
/// reject at execution time.
async fn validate(ApiJson(req): ApiJson<ExplainReq>) -> impl IntoResponse {
    match sqlserver::parse_check(&req.conn, &req.sql).await {
        Ok(mut diags) => {
            if diags.is_empty() {
                diags = implicit_exec_diagnostics(&req.sql);
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "ok": diags.is_empty(),
                    "checked_by": "SET PARSEONLY ON on the connected server (syntax only; object names are not bound) plus a first-keyword check",
                    "diagnostics": diags,
                })),
            )
                .into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, Json(serde_json::json!({ "error": e.to_string() }))).into_response(),
    }
}

/// Words that may legitimately open a T-SQL batch. Anything else that appears
/// as a bare, unqualified first token is a name the server would treat as an
/// implicit `EXEC` — almost always a misspelled keyword.
const BATCH_OPENERS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "WITH", "DECLARE", "SET", "IF", "ELSE",
    "WHILE", "BEGIN", "END", "BREAK", "CONTINUE", "RETURN", "GOTO", "WAITFOR", "TRY", "CATCH",
    "THROW", "RAISERROR", "PRINT", "EXEC", "EXECUTE", "CREATE", "ALTER", "DROP", "TRUNCATE",
    "USE", "GRANT", "REVOKE", "DENY", "OPEN", "CLOSE", "FETCH", "DEALLOCATE", "COMMIT",
    "ROLLBACK", "SAVE", "BACKUP", "RESTORE", "DBCC", "BULK", "ENABLE", "DISABLE", "KILL",
    "CHECKPOINT", "RECONFIGURE", "SHUTDOWN", "REVERT", "SEND", "RECEIVE", "MOVE", "GET",
    "READTEXT", "WRITETEXT", "UPDATETEXT", "SETUSER", "ADD", "OPENROWSET", "VALUES", "GO",
];

/// Split the text into GO batches, and for the FIRST statement of each batch
/// flag a bare first token that is neither a T-SQL statement keyword nor
/// something that plausibly names a procedure (`sp_`/`xp_` prefix, a
/// schema-qualified or bracketed name, a variable, a label). `line` is the
/// 1-based line of the offending token within the submitted text.
fn implicit_exec_diagnostics(sql: &str) -> Vec<sqlserver::ParseDiagnostic> {
    let mut out = Vec::new();
    let mut batch_start_line = 1usize;
    let mut batch = String::new();
    let mut line_no = 0usize;
    let mut flush = |batch: &mut String, start: usize, out: &mut Vec<sqlserver::ParseDiagnostic>| {
        if let Some(d) = first_token_diagnostic(batch, start) {
            out.push(d);
        }
        batch.clear();
    };
    for line in sql.lines() {
        line_no += 1;
        let t = line.trim();
        let is_go = t.get(..2).is_some_and(|w| w.eq_ignore_ascii_case("go"))
            && t[2..].trim_start().chars().next().map_or(true, |c| c.is_ascii_digit() || c == '-');
        if is_go {
            flush(&mut batch, batch_start_line, &mut out);
            batch_start_line = line_no + 1;
            continue;
        }
        batch.push_str(line);
        batch.push('\n');
    }
    flush(&mut batch, batch_start_line, &mut out);
    out
}

fn first_token_diagnostic(batch: &str, start_line: usize) -> Option<sqlserver::ParseDiagnostic> {
    // Skip whitespace, `--` line comments and `/* */` block comments, counting lines.
    let b = batch.as_bytes();
    let mut i = 0usize;
    let mut line = start_line;
    loop {
        while i < b.len() && (b[i] as char).is_whitespace() {
            if b[i] == b'\n' { line += 1; }
            i += 1;
        }
        if b[i..].starts_with(b"--") {
            while i < b.len() && b[i] != b'\n' { i += 1; }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                if b[i] == b'\n' { line += 1; }
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        break;
    }
    let rest = &batch[i..];
    let first = rest.chars().next()?;
    // Not a bare word: `(`, `;`, `[name]`, `@var`, `"quoted"`, a number… — the
    // server's own parser is the authority for all of these.
    if !(first.is_ascii_alphabetic() || first == '_' || first == '#') {
        return None;
    }
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '#' || *c == '$')
        .collect();
    let after = rest[word.len()..].chars().next();
    let upper = word.to_ascii_uppercase();
    if BATCH_OPENERS.contains(&upper.as_str()) {
        return None;
    }
    // A plausible procedure call: `sp_…`, `xp_…`, `dbo.proc`, `proc_name`, or a label `retry:`.
    let lower = word.to_ascii_lowercase();
    if lower.starts_with("sp_") || lower.starts_with("xp_") || lower.starts_with("usp_")
        || lower.starts_with('#') || lower.contains('_') || after == Some('.') || after == Some(':')
    {
        return None;
    }
    Some(sqlserver::ParseDiagnostic {
        number: 0,
        line: line as u32,
        message: format!(
            "'{word}' is not a T-SQL statement keyword. The server accepted it only because a bare name opening a batch is read as an implicit procedure call (EXEC {word} …), which PARSEONLY does not verify. If {word} is a procedure, write EXEC {word} explicitly; otherwise fix the spelling."
        ),
    })
}

#[cfg(test)]
mod validate_gate_tests {
    use super::implicit_exec_diagnostics;

    #[test]
    fn misspelled_first_keyword_is_flagged() {
        for sql in ["SELCT 1", "SELCT 1, 2", "-- note\nSELCT 1", "/* x */ UPDAT t SET a = 1", "  Selct"] {
            let d = implicit_exec_diagnostics(sql);
            assert_eq!(d.len(), 1, "{sql:?} should be flagged");
            assert_eq!(d[0].number, 0);
        }
        assert_eq!(implicit_exec_diagnostics("-- c\nSELCT 1")[0].line, 2);
        assert_eq!(implicit_exec_diagnostics("SELECT 1\nGO\nSELCT 2")[0].line, 3);
    }

    #[test]
    fn real_openers_and_procedure_calls_pass() {
        for sql in [
            "SELECT 1", "select 1", "WITH c AS (SELECT 1 x) SELECT * FROM c", "DECLARE @a INT; SET @a = 1",
            "sp_who", "dbo.usp_Report @d = 1", "[dbo].[proc]", "@x = 1", "(SELECT 1)", ";", "",
            "BEGIN TRAN", "retry:\nSELECT 1", "EXEC my_proc", "my_proc 1, 2", "GO", "  \n  ",
            "SELECT 1\nGO\nSELECT 2\nGO 5",
        ] {
            assert!(implicit_exec_diagnostics(sql).is_empty(), "{sql:?} should pass");
        }
    }
}

/// POST /api/qstore/status — Query Store config for the connected database.
async fn qstore_status(ApiJson(req): ApiJson<ConnectReq>) -> impl IntoResponse {
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

async fn qstore_top(ApiJson(req): ApiJson<QStoreTopReq>) -> impl IntoResponse {
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
    /// Must be `true`. The confirmation prompt lives in the UI, but a prompt is
    /// a convention, not a boundary: this endpoint is reachable by anything
    /// that can send a request to the loopback port. Requiring the flag on the
    /// wire means the only DDL dbopt can issue cannot be triggered by accident
    /// or by a page the user merely visited.
    #[serde(default)]
    pub confirmed: bool,
}

/// POST /api/qstore/capture — set the connected DB's Query Store capture mode.
/// This runs DDL; the UI must preview the statement and get explicit user
/// confirmation first (Safe-Apply). Mode is allowlisted in `set_query_store_capture`.
async fn qstore_capture(ApiJson(req): ApiJson<QStoreCaptureReq>) -> impl IntoResponse {
    if !req.confirmed {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "this endpoint changes database settings and requires an explicit confirmation from the user"
            })),
        )
            .into_response();
    }
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

async fn llm_chat(ApiJson(req): ApiJson<ChatReq>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
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
    ApiJson(req): ApiJson<CloudChatReq>,
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
async fn llm_cloud_models(Path(provider): Path<String>, ApiJson(req): ApiJson<DiscoverReq>) -> axum::response::Response {
    match providers::discover::list_models(&provider, &req.config).await {
        Ok(models) => (StatusCode::OK, Json(serde_json::json!({ "models": models }))).into_response(),
        Err(e) => discover_err(e),
    }
}

/// Validate a cloud provider API key (and, for OpenRouter, report credits).
async fn llm_cloud_test(Path(provider): Path<String>, ApiJson(req): ApiJson<DiscoverReq>) -> axum::response::Response {
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

async fn analyze(ApiJson(req): ApiJson<AnalyzeReq>) -> impl IntoResponse {
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
