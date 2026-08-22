//! sentinel — continuous monitoring daemon for SQL Server instances.
//!
//! Polls a configurable set of DMV/Query-Store surfaces on each registered
//! instance and persists the rolled-up rows into a single bundled SQLite
//! file. The HTTP/UI layer reads from that store; the daemon never blocks
//! on the consumer.
//!
//! Real poller bodies live in `src/poll/`. This module just owns the public
//! handle (`Sentinel`), the configuration types, and the lifecycle.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

pub mod alerts;
pub mod conn;
pub mod notify;
pub mod poll;
pub mod probes;
pub mod report;
pub mod scheduler;
pub mod storage;

pub use alerts::{AlertConfig, AlertRule, Comparator, FiredAlert, Severity, WebhookFormat};
pub use storage::Storage;

/// Connection info for a SQL Server instance the sentinel should poll.
///
/// Mirrors the shape of `backend::routes::ConnectReq` so we can copy values
/// from the UI without a cross-crate dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub server: String,
    pub database: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub trust_cert: Option<bool>,
}

/// Per-surface polling cadences. Each value is the interval between polls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cadences {
    /// `sys.dm_exec_requests` / live blocking — seconds.
    pub live_secs: u64,
    /// Query Store rollup — minutes.
    pub query_store_mins: u64,
    /// `sys.dm_os_wait_stats` delta — minutes.
    pub waits_mins: u64,
    /// `sys.dm_db_index_usage_stats` delta — minutes.
    pub index_usage_mins: u64,
    /// Deep live vitals (CPU/scheduler pressure, memory headroom, file-IO
    /// latency deltas, tempdb contention, plan-cache health) — seconds. These
    /// are cheap DMV reads, so they default to a tight cadence like the live
    /// poller. `#[serde(default)]` so configs written before this field still
    /// deserialize and pick up the default.
    #[serde(default = "default_vitals_secs")]
    pub vitals_secs: u64,
}

/// Default cadence for the deep-vitals pollers (seconds).
pub fn default_vitals_secs() -> u64 { 60 }

impl Default for Cadences {
    fn default() -> Self {
        Self {
            live_secs: 60,
            query_store_mins: 5,
            waits_mins: 5,
            index_usage_mins: 15,
            vitals_secs: default_vitals_secs(),
        }
    }
}

/// One monitored SQL Server instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub name: String,
    pub conn: ConnectionInfo,
    #[serde(default)]
    pub cadences: Cadences,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool { true }

/// Daemon-level configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelConfig {
    pub instances: Vec<InstanceConfig>,
    pub db_path: PathBuf,
    /// How many days of captured time-series to keep. Older rows are pruned by a
    /// background housekeeping task so the SQLite store can't grow without bound.
    /// `0` disables pruning (keep forever).
    #[serde(default = "default_retention_days")]
    pub retention_days: u64,
    /// Threshold alerting: webhook + armed rules. `#[serde(default)]` so configs
    /// written before alerting existed still deserialize and pick up the grounded
    /// SPEC rule set (see `alerts::default_rules`).
    #[serde(default)]
    pub alerting: alerts::AlertConfig,
    /// How often (seconds) the alert-evaluation pass reads back the latest
    /// persisted vitals and checks the rules. Defaults to the vitals cadence.
    #[serde(default = "default_vitals_secs")]
    pub alert_eval_secs: u64,
}

/// Default retention window for captured telemetry (90 days).
pub fn default_retention_days() -> u64 { 90 }

impl SentinelConfig {
    /// Resolve the default DB path: `$DBOPT_DATA_DIR/sentinel.db` if set,
    /// otherwise `~/.dbopt/sentinel.db`.
    pub fn default_db_path() -> PathBuf {
        if let Ok(dir) = std::env::var("DBOPT_DATA_DIR") {
            return PathBuf::from(dir).join("sentinel.db");
        }
        let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let current = base.join(".dbopt");
        // Migration shim: if the new ~/.dbopt dir doesn't exist yet but the
        // pre-rebrand ~/.sqlopt does, keep using it so existing monitoring data
        // and the autostart config survive the rename.
        let legacy = base.join(".sqlopt");
        let dir = if !current.exists() && legacy.exists() {
            // Logged once per process: default_db_path() is called from several
            // request handlers, and a DBA deciding what to back up or purge
            // must be able to find this line, not 400 copies of it.
            static LOGGED: std::sync::Once = std::sync::Once::new();
            LOGGED.call_once(|| {
                tracing::info!(
                    target: "sentinel",
                    "data dir: using legacy {} because {} does not exist (nothing moved; rename the folder to switch)",
                    legacy.display(),
                    current.display()
                );
            });
            legacy
        } else {
            current
        };
        dir.join("sentinel.db")
    }
}

/// Handle to a running sentinel daemon. Drop or call `stop` to shut down.
pub struct Sentinel {
    storage: Arc<Storage>,
    shutdown: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

impl Sentinel {
    /// Spawn pollers for every enabled instance in `config`.
    ///
    /// Opens (or creates) the SQLite store and runs pending migrations
    /// before any task is launched, so a misconfigured DB fails fast.
    pub async fn start(config: SentinelConfig) -> anyhow::Result<Self> {
        if let Some(parent) = config.db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let storage = Arc::new(Storage::open(&config.db_path)?);
        let shutdown = CancellationToken::new();

        let storage_for_task = storage.clone();
        let shutdown_for_task = shutdown.clone();
        let instances = config.instances.clone();
        let retention_days = config.retention_days;
        let alerting = config.alerting.clone();
        let alert_eval_secs = config.alert_eval_secs;
        let join = tokio::spawn(async move {
            scheduler::run(scheduler::RunConfig {
                instances,
                storage: storage_for_task,
                shutdown: shutdown_for_task,
                retention_days,
                alerting,
                alert_eval_secs,
            })
            .await;
        });

        tracing::info!(target: "sentinel", "sentinel started with {} instance(s)", config.instances.len());
        Ok(Self { storage, shutdown, join })
    }

    /// Borrow the underlying storage for ad-hoc reads (reports, UI).
    pub fn storage(&self) -> &Arc<Storage> { &self.storage }

    /// Signal every poller to stop and await scheduler shutdown.
    pub async fn stop(self) {
        self.shutdown.cancel();
        let _ = self.join.await;
        tracing::info!(target: "sentinel", "sentinel stopped");
    }
}
