//! Backend HTTP handlers that bridge the `sentinel` crate into the
//! Observatory web UI.
//!
//! Lifecycle is owned by a process-global `OnceCell<Mutex<Option<Sentinel>>>`
//! — we want at most one daemon per backend process. Report endpoints open the
//! SQLite store read-only via `Storage::open` so they keep working even when
//! the daemon is stopped.

use crate::routes::ApiJson;
use axum::{
    extract::{Json, Query},
    http::{header, StatusCode},
    response::IntoResponse,
};
use sentinel::{
    alerts::AlertConfig,
    report::{render_html, render_markdown, render_weekly, WeeklyReport},
    storage::{Storage, TimeRange, VitalMetric},
    ConnectionInfo, InstanceConfig, Sentinel, SentinelConfig,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};

/// Process-global handle. `None` inside the `Mutex` means "not running".
static DAEMON: OnceCell<Arc<Mutex<Option<Sentinel>>>> = OnceCell::const_new();

async fn daemon_slot() -> Arc<Mutex<Option<Sentinel>>> {
    DAEMON
        .get_or_init(|| async { Arc::new(Mutex::new(None)) })
        .await
        .clone()
}

// ---------- persisted config ----------------------------------------------

/// Where we persist the daemon's instance configuration so monitoring can
/// resume after a backend restart. Lives next to the SQLite store at
/// `~/.dbopt/sentinel-config.json`.
fn config_path() -> PathBuf {
    let db_path = SentinelConfig::default_db_path();
    let dir = db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("sentinel-config.json")
}

/// On-disk shape for sentinel autostart. `autostart` gates whether the daemon
/// is relaunched on boot; `instances` is kept even when autostart is off so the
/// user can re-enable without re-entering connection details.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedConfig {
    autostart: bool,
    instances: Vec<PersistedInstance>,
    /// Threshold-alerting config (webhook + rules). `#[serde(default)]` so configs
    /// written before alerting existed still load and pick up the grounded SPEC
    /// defaults; the user's edits are persisted here and survive a restart.
    #[serde(default)]
    alerting: AlertConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedInstance {
    name: String,
    conn: ConnectionInfo,
}

/// Best-effort write of the persisted config; logs a warning on failure.
///
/// This file holds the DB connection password in plaintext, so on Unix we lock
/// down the directory (0700) and file (0600) to the owning user. On Windows we
/// rely on the default per-user profile ACLs.
fn write_persisted(cfg: &PersistedConfig) {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let path = config_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(target: "sentinel", "failed to create config dir {}: {e}", parent.display());
            return;
        }
        #[cfg(unix)]
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    match serde_json::to_string_pretty(cfg) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!(target: "sentinel", "failed to write sentinel config {}: {e}", path.display());
            } else {
                #[cfg(unix)]
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
        Err(e) => tracing::warn!(target: "sentinel", "failed to serialize sentinel config: {e}"),
    }
}

/// Best-effort read of the persisted config (instances + alerting). `None` when
/// the file is missing or unparseable.
fn read_persisted() -> Option<PersistedConfig> {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<PersistedConfig>(&raw).ok())
}

/// Build a `SentinelConfig` from persisted instances + alerting config with
/// default cadences.
fn build_config(instances: Vec<PersistedInstance>, alerting: AlertConfig) -> SentinelConfig {
    SentinelConfig {
        instances: instances
            .into_iter()
            .map(|i| InstanceConfig {
                name: i.name,
                conn: i.conn,
                cadences: Default::default(),
                enabled: true,
            })
            .collect(),
        db_path: SentinelConfig::default_db_path(),
        retention_days: sentinel::default_retention_days(),
        alerting,
        alert_eval_secs: sentinel::default_vitals_secs(),
    }
}

/// If a persisted config with `autostart == true` and at least one instance
/// exists, relaunch the daemon into the shared slot. Never panics or blocks
/// boot — every failure path just logs.
pub async fn autostart_from_disk() {
    let path = config_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => {
            tracing::info!(target: "sentinel", "no persisted sentinel config at {}, skipping autostart", path.display());
            return;
        }
    };
    let persisted: PersistedConfig = match serde_json::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(target: "sentinel", "failed to parse sentinel config {}: {e}", path.display());
            return;
        }
    };
    if !persisted.autostart || persisted.instances.is_empty() {
        tracing::info!(target: "sentinel", "sentinel autostart disabled or no instances; skipping");
        return;
    }

    let slot = daemon_slot().await;
    let mut guard = slot.lock().await;
    if guard.is_some() {
        return;
    }
    let count = persisted.instances.len();
    let cfg = build_config(persisted.instances, persisted.alerting);
    match Sentinel::start(cfg).await {
        Ok(s) => {
            *guard = Some(s);
            tracing::info!(target: "sentinel", "autostarted sentinel from disk with {count} instance(s)");
        }
        Err(e) => {
            tracing::warn!(target: "sentinel", "sentinel autostart failed: {e}");
        }
    }
}

// ---------- request shapes -------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StartReq {
    pub instances: Vec<InstanceReq>,
}

#[derive(Debug, Deserialize)]
pub struct InstanceReq {
    pub name: String,
    pub conn: ConnectionInfo,
}

#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    #[serde(default)]
    pub days: Option<i64>,
    /// Which query sort the user has selected in the UI ("duration" | "recent").
    /// Echoed into the report so HTML/Markdown lead with that view and JSON
    /// reflects it — a download then matches what the user is looking at.
    #[serde(default)]
    pub sort: Option<String>,
}

fn window_from_query(q: &ReportQuery) -> TimeRange {
    let days = q.days.unwrap_or(7).max(1);
    TimeRange::last_days(days)
}

/// Normalize the requested sort to the allowed set; anything but "recent" is
/// treated as the default "duration".
fn sort_from_query(q: &ReportQuery) -> String {
    match q.sort.as_deref() {
        Some(s) if s.eq_ignore_ascii_case("recent") => "recent",
        _ => "duration",
    }
    .to_string()
}

// ---------- handlers -------------------------------------------------------

pub async fn start(ApiJson(req): ApiJson<StartReq>) -> impl IntoResponse {
    let slot = daemon_slot().await;
    let mut guard = slot.lock().await;
    if guard.is_some() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "already_running": true })),
        )
            .into_response();
    }

    let persisted_instances: Vec<PersistedInstance> = req
        .instances
        .into_iter()
        .map(|i| PersistedInstance {
            name: i.name,
            conn: i.conn,
        })
        .collect();

    // Carry forward an existing alerting config (webhook + edited rules) so
    // starting the daemon doesn't wipe the user's thresholds; default otherwise.
    let alerting = read_persisted().map(|p| p.alerting).unwrap_or_default();
    let cfg = build_config(persisted_instances.clone(), alerting.clone());

    match Sentinel::start(cfg).await {
        Ok(s) => {
            *guard = Some(s);
            // Persist so the daemon resumes after a backend restart. Best-effort.
            write_persisted(&PersistedConfig {
                autostart: true,
                instances: persisted_instances,
                alerting,
            });
            (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "already_running": false })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn stop() -> impl IntoResponse {
    let slot = daemon_slot().await;
    let mut guard = slot.lock().await;
    if let Some(s) = guard.take() {
        s.stop().await;
    }
    // Disable autostart but keep the instances + alerting config so the user can
    // re-enable without re-entering anything. Best-effort.
    let prior = read_persisted();
    write_persisted(&PersistedConfig {
        autostart: false,
        instances: prior.as_ref().map(|p| p.instances.clone()).unwrap_or_default(),
        alerting: prior.map(|p| p.alerting).unwrap_or_default(),
    });
    (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
}

pub async fn status() -> impl IntoResponse {
    let slot = daemon_slot().await;
    let guard = slot.lock().await;
    let running = guard.is_some();
    let db_path = SentinelConfig::default_db_path();
    let instances = match Storage::open(&db_path) {
        Ok(s) => s.instance_count().unwrap_or(0),
        Err(_) => 0,
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "running": running,
            "db_path": tilde_path(&db_path),
            "instances": instances,
        })),
    )
}

/// Build a `WeeklyReport` for the configured window, or an empty stub if the
/// DB hasn't been created yet (e.g. user hit the report tab before ever
/// starting the daemon).
/// Seconds of captured telemetry currently held (None if the sentinel store
/// doesn't exist yet or is empty). Lets the Health front-door distinguish a
/// just-started / freshly-reset monitor from a long, genuinely-clean history.
/// Collapse the user's home directory to `~` before sending a path to the UI.
///
/// The status endpoint is how a user finds their own database file, so the path
/// is genuinely useful — but the absolute form carries the OS account name, and
/// this response is readable by anything that can reach the port.
fn tilde_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = home.to_string_lossy().to_string();
        if !home.is_empty() {
            if let Some(rest) = s.strip_prefix(&home) {
                return format!("~{rest}");
            }
        }
    }
    s
}

pub fn monitoring_age_secs() -> Option<i64> {
    let path = SentinelConfig::default_db_path();
    Storage::open(&path).ok().and_then(|s| s.monitoring_age_secs())
}

/// Seconds since the NEWEST sentinel capture (`now - MAX(captured_at)`), or
/// `None` when nothing was ever captured. Tells the Health front-door whether
/// the telemetry is live or a fossil — `monitoring_age_secs` alone cannot.
pub fn last_capture_secs() -> Option<i64> {
    let path = SentinelConfig::default_db_path();
    Storage::open(&path).ok().and_then(|s| s.last_capture_secs())
}

/// Read the most-recent DEEP-VITALS sample of each surface for `server` out of
/// the sentinel store, shaped for the live UI's "DEEP VITALS" panel.
///
/// Honest empty state, never an error: if the store doesn't exist yet, or the
/// server has never been monitored (no instance row), every surface is `null`
/// (I/O latency `[]`) and `has_data` is `false`. `captured_at` is the newest
/// instant across all surfaces present (epoch millis), or `null` when empty —
/// the UI shows it as "as of …".
pub fn deep_vitals(server: &str) -> serde_json::Value {
    let empty = || {
        serde_json::json!({
            "has_data": false,
            "captured_at": serde_json::Value::Null,
            "cpu_pressure": serde_json::Value::Null,
            "memory_headroom": serde_json::Value::Null,
            "io_latency": [],
            "tempdb_contention": serde_json::Value::Null,
            "plan_cache": serde_json::Value::Null,
            // Same shape as the populated path so the UI never special-cases a
            // missing key — every series is just an empty list.
            "series": {
                "cpu_runnable_tasks": [],
                "memory_ple": [],
                "plan_cache_single_use": [],
                "tempdb_total_waiters": [],
                "io_worst_latency_ms": [],
            },
        })
    };

    let path = SentinelConfig::default_db_path();
    let storage = match Storage::open(&path) {
        Ok(s) => s,
        Err(_) => return empty(), // store not created yet → not an error
    };
    let Some(instance_id) = storage.get_instance_id(server) else {
        return empty(); // server never monitored → not an error
    };

    let cpu = storage.latest_cpu_pressure(instance_id);
    let mem = storage.latest_memory_headroom(instance_id);
    let io = storage.latest_io_latency(instance_id);
    let tempdb = storage.latest_tempdb_contention(instance_id);
    let plan = storage.latest_plan_cache(instance_id);

    // Recent trend behind each headline scalar — the sparkline source. Each is a
    // list of [captured_at_ms, value] pairs, oldest→newest (freshest last),
    // capped to the last `SERIES_LIMIT` captures. Empty when nothing recorded.
    const SERIES_LIMIT: usize = 60;
    let series = |m: VitalMetric| -> serde_json::Value {
        serde_json::Value::Array(
            storage
                .recent_vital_series(instance_id, m, SERIES_LIMIT)
                .into_iter()
                .map(|(at, v)| serde_json::json!([at, v]))
                .collect(),
        )
    };
    let io_series = serde_json::Value::Array(
        storage
            .recent_io_latency_series(instance_id, SERIES_LIMIT)
            .into_iter()
            .map(|(at, v)| serde_json::json!([at, v]))
            .collect(),
    );
    let series = serde_json::json!({
        "cpu_runnable_tasks": series(VitalMetric::CpuRunnableTasks),
        "memory_ple": series(VitalMetric::MemoryPle),
        "plan_cache_single_use": series(VitalMetric::PlanCacheSingleUse),
        "tempdb_total_waiters": series(VitalMetric::TempdbTotalWaiters),
        "io_worst_latency_ms": io_series,
    });

    // Newest captured_at across whichever surfaces have data (millis).
    let captured_at_ms = [
        cpu.as_ref().map(|r| r.captured_at.timestamp_millis()),
        mem.as_ref().map(|r| r.captured_at.timestamp_millis()),
        io.first().map(|r| r.captured_at.timestamp_millis()),
        tempdb.as_ref().map(|r| r.captured_at.timestamp_millis()),
        plan.as_ref().map(|r| r.captured_at.timestamp_millis()),
    ]
    .into_iter()
    .flatten()
    .max();

    let has_data =
        cpu.is_some() || mem.is_some() || !io.is_empty() || tempdb.is_some() || plan.is_some();

    // The row structs derive Serialize, so each maps straight to JSON; the field
    // names match the storage row structs (snake_case), which the UI consumes.
    serde_json::json!({
        "has_data": has_data,
        "captured_at": captured_at_ms,
        "cpu_pressure": cpu,
        "memory_headroom": mem,
        "io_latency": io,
        "tempdb_contention": tempdb,
        "plan_cache": plan,
        "series": series,
    })
}

/// Read the "today vs rolling baseline" summary for `server` out of the durable
/// query-baseline table, for the health-grade trend badge. `None` when the store
/// doesn't exist, the server was never monitored, or no query has accumulated a
/// mature baseline yet — the UI then renders "baseline forming" rather than a
/// fabricated delta. Read-only; never touches the live server.
///
/// Scoped to the DATABASE being graded: when `database` is given, only the
/// instance monitored for that exact server+database pair is consulted, and a
/// miss is `None` — never another database's baseline wearing this one's
/// badge. Without a database the server's newest instance is used.
pub fn health_baseline_summary(
    server: &str,
    database: Option<&str>,
) -> Option<sentinel::storage::HealthBaselineSummary> {
    let path = SentinelConfig::default_db_path();
    let storage = Storage::open(&path).ok()?;
    let instance_id = match database {
        Some(db) if !db.is_empty() => storage.get_instance_id_for_db(server, db)?,
        _ => storage.get_instance_id(server)?,
    };
    storage.health_baseline_summary(instance_id)
}

pub fn build_report(window: TimeRange) -> WeeklyReport {
    let path = SentinelConfig::default_db_path();
    match Storage::open(&path) {
        Ok(storage) => render_weekly(&storage, window),
        Err(_) => WeeklyReport {
            window_from: window.from,
            window_to: window.to,
            instances: 0,
            pain: Default::default(),
            top_queries: Vec::new(),
            recent_queries: Vec::new(),
            regressions: Vec::new(),
            unused_indexes: Vec::new(),
            sort: "duration".to_string(),
        },
    }
}

pub async fn report_json(Query(q): Query<ReportQuery>) -> impl IntoResponse {
    let window = window_from_query(&q);
    let mut report = build_report(window);
    report.sort = sort_from_query(&q);
    (StatusCode::OK, Json(report))
}

pub async fn report_markdown(Query(q): Query<ReportQuery>) -> impl IntoResponse {
    let window = window_from_query(&q);
    let mut report = build_report(window);
    report.sort = sort_from_query(&q);
    let body = render_markdown(&report);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        body,
    )
}

pub async fn report_html(Query(q): Query<ReportQuery>) -> impl IntoResponse {
    let window = window_from_query(&q);
    let mut report = build_report(window);
    report.sort = sort_from_query(&q);
    let body = render_html(&report);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
}

// ---------- alerts ---------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AlertsQuery {
    /// How many recent fired alerts to return (default 50, capped at 500).
    #[serde(default)]
    pub limit: Option<i64>,
}

/// GET /api/alerts — the most-recent fired alerts read back from the sentinel
/// store, newest first. Read-only: opens the SQLite store and reads `alerts_fired`
/// — never touches a live server. Returns `{ alerts: [] }` (200, not an error)
/// when the store doesn't exist yet or nothing has fired — an honest empty state.
pub async fn alerts(Query(q): Query<AlertsQuery>) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let path = SentinelConfig::default_db_path();
    let alerts = match Storage::open(&path) {
        Ok(s) => s.recent_alerts(limit).unwrap_or_default(),
        Err(_) => Vec::new(), // store not created yet → not an error
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({ "alerts": alerts })),
    )
}

/// GET /api/alerts/config — the current alerting config (webhook + rules). Reads
/// the persisted config; falls back to the grounded SPEC defaults when none has
/// been written yet, so the UI always renders the armed rule set.
pub async fn get_alert_config() -> impl IntoResponse {
    let alerting = read_persisted().map(|p| p.alerting).unwrap_or_default();
    (StatusCode::OK, Json(alerting))
}

/// POST /api/alerts/config — update the alerting config (webhook + rules). The
/// new config is persisted to `sentinel-config.json` AND, if the daemon is
/// running, it is restarted with the new config so the change takes effect
/// without the user having to stop/start manually.
pub async fn set_alert_config(ApiJson(alerting): ApiJson<AlertConfig>) -> impl IntoResponse {
    // Preserve the existing instances + autostart flag; only the alerting block
    // changes here.
    let prior = read_persisted();
    let instances = prior.as_ref().map(|p| p.instances.clone()).unwrap_or_default();
    let autostart = prior.as_ref().map(|p| p.autostart).unwrap_or(false);
    write_persisted(&PersistedConfig {
        autostart,
        instances: instances.clone(),
        alerting: alerting.clone(),
    });

    // If the daemon is live, hot-reload it so the new thresholds apply now.
    let slot = daemon_slot().await;
    let mut guard = slot.lock().await;
    let mut reloaded = false;
    if guard.is_some() {
        if let Some(s) = guard.take() {
            s.stop().await;
        }
        let cfg = build_config(instances, alerting);
        match Sentinel::start(cfg).await {
            Ok(s) => {
                *guard = Some(s);
                reloaded = true;
            }
            Err(e) => {
                tracing::warn!(target: "sentinel", "failed to restart with new alert config: {e}");
            }
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "reloaded": reloaded })),
    )
}
