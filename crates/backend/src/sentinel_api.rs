//! Backend HTTP handlers that bridge the `sentinel` crate into the
//! Observatory web UI.
//!
//! Lifecycle is owned by a process-global `OnceCell<Mutex<Option<Sentinel>>>`
//! — we want at most one daemon per backend process. Report endpoints open the
//! SQLite store read-only via `Storage::open` so they keep working even when
//! the daemon is stopped.

use axum::{
    extract::{Json, Query},
    http::{header, StatusCode},
    response::IntoResponse,
};
use sentinel::{
    report::{render_html, render_markdown, render_weekly, WeeklyReport},
    storage::{Storage, TimeRange},
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
/// `~/.sqlopt/sentinel-config.json`.
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

/// Build a `SentinelConfig` from persisted instances with default cadences.
fn build_config(instances: Vec<PersistedInstance>) -> SentinelConfig {
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
    let cfg = build_config(persisted.instances);
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
}

fn window_from_query(q: &ReportQuery) -> TimeRange {
    let days = q.days.unwrap_or(7).max(1);
    TimeRange::last_days(days)
}

// ---------- handlers -------------------------------------------------------

pub async fn start(Json(req): Json<StartReq>) -> impl IntoResponse {
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

    let cfg = build_config(persisted_instances.clone());

    match Sentinel::start(cfg).await {
        Ok(s) => {
            *guard = Some(s);
            // Persist so the daemon resumes after a backend restart. Best-effort.
            write_persisted(&PersistedConfig {
                autostart: true,
                instances: persisted_instances,
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
    // Disable autostart but keep the instances so the user can re-enable.
    // Best-effort: read the existing config (if any) to preserve instances.
    let instances = std::fs::read_to_string(config_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<PersistedConfig>(&raw).ok())
        .map(|p| p.instances)
        .unwrap_or_default();
    write_persisted(&PersistedConfig {
        autostart: false,
        instances,
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
            "db_path": db_path.display().to_string(),
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
pub fn monitoring_age_secs() -> Option<i64> {
    let path = SentinelConfig::default_db_path();
    Storage::open(&path).ok().and_then(|s| s.monitoring_age_secs())
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
        },
    }
}

pub async fn report_json(Query(q): Query<ReportQuery>) -> impl IntoResponse {
    let window = window_from_query(&q);
    let report = build_report(window);
    (StatusCode::OK, Json(report))
}

pub async fn report_markdown(Query(q): Query<ReportQuery>) -> impl IntoResponse {
    let window = window_from_query(&q);
    let report = build_report(window);
    let body = render_markdown(&report);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        body,
    )
}

pub async fn report_html(Query(q): Query<ReportQuery>) -> impl IntoResponse {
    let window = window_from_query(&q);
    let report = build_report(window);
    let body = render_html(&report);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
}
