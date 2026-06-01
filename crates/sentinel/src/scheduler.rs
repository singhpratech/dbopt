//! Per-instance, per-surface task scheduler.
//!
//! For every enabled instance we spawn one tokio task per surface. Each
//! task ticks on its configured interval and invokes the corresponding
//! poller. A `CancellationToken` lets `Sentinel::stop` cooperatively
//! shut the whole tree down.
//!
//! Poller errors are caught and logged so a single failure doesn't kill
//! the loop — the next tick gets a clean attempt.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::{interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::poll;
use crate::storage::Storage;
use crate::{ConnectionInfo, InstanceConfig};

/// Top-level driver. Returns once every spawned task has exited (i.e. the
/// shutdown token was cancelled).
pub async fn run(
    instances: Vec<InstanceConfig>,
    storage: Arc<Storage>,
    shutdown: CancellationToken,
    retention_days: u64,
) {
    let mut handles = Vec::new();

    // Housekeeping: prune aged-out telemetry so the SQLite store can't grow
    // without bound. Fires immediately on start (trims an existing large DB),
    // then every 6 hours. `retention_days == 0` disables pruning.
    if retention_days > 0 {
        let storage = storage.clone();
        let shutdown = shutdown.clone();
        handles.push(tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(6 * 60 * 60));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);
                        match storage.prune_before(cutoff) {
                            Ok(n) if n > 0 => tracing::info!(
                                target: "sentinel::scheduler",
                                "pruned {n} telemetry rows older than {retention_days}d"
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(
                                target: "sentinel::scheduler", "prune failed: {e:#}"
                            ),
                        }
                    }
                }
            }
        }));
    }

    for inst in instances.into_iter().filter(|i| i.enabled) {
        let cadences = inst.cadences.clone();
        let conn = Arc::new(inst.conn.clone());
        let name = inst.name.clone();

        handles.push(spawn_poller(
            format!("{name}/live"),
            Duration::from_secs(cadences.live_secs.max(1)),
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::live::poll_live_requests(&c, &s).await
            }),
        ));
        handles.push(spawn_poller(
            format!("{name}/query_store"),
            Duration::from_secs(cadences.query_store_mins.max(1) * 60),
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::query_store::poll_query_store(&c, &s).await
            }),
        ));
        handles.push(spawn_poller(
            format!("{name}/waits"),
            Duration::from_secs(cadences.waits_mins.max(1) * 60),
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::waits::poll_wait_stats(&c, &s).await
            }),
        ));
        handles.push(spawn_poller(
            format!("{name}/deadlocks"),
            Duration::from_secs(cadences.waits_mins.max(1) * 60),
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::deadlocks::poll_deadlocks(&c, &s).await
            }),
        ));
        handles.push(spawn_poller(
            format!("{name}/index_usage"),
            Duration::from_secs(cadences.index_usage_mins.max(1) * 60),
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::index_usage::poll_index_usage_delta(&c, &s).await
            }),
        ));
        handles.push(spawn_poller(
            format!("{name}/sizes"),
            Duration::from_secs(cadences.index_usage_mins.max(1) * 60),
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::sizes::poll_sizes(&c, &s).await
            }),
        ));

        // ---- deep live vitals (the community real-time script-style real-time pressure) ----
        // Cheap DMV reads on a tight cadence, each in its own task so one
        // missing-permission surface can't starve the others.
        let vitals_period = Duration::from_secs(cadences.vitals_secs.max(1));
        handles.push(spawn_poller(
            format!("{name}/cpu_pressure"),
            vitals_period,
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::cpu_pressure::poll_cpu_pressure(&c, &s).await
            }),
        ));
        handles.push(spawn_poller(
            format!("{name}/memory_headroom"),
            vitals_period,
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::memory_headroom::poll_memory_headroom(&c, &s).await
            }),
        ));
        handles.push(spawn_poller(
            format!("{name}/io_latency"),
            vitals_period,
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::io_latency::poll_io_latency(&c, &s).await
            }),
        ));
        handles.push(spawn_poller(
            format!("{name}/tempdb_contention"),
            vitals_period,
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::tempdb_contention::poll_tempdb_contention(&c, &s).await
            }),
        ));
        handles.push(spawn_poller(
            format!("{name}/plan_cache"),
            vitals_period,
            conn.clone(),
            storage.clone(),
            shutdown.clone(),
            |c, s| Box::pin(async move {
                poll::plan_cache::poll_plan_cache(&c, &s).await
            }),
        ));
    }

    for h in handles {
        let _ = h.await;
    }
}

type PollFut = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>;

/// Spawn a single ticking poller. The closure is invoked on every tick
/// with cloned `Arc`s — pollers borrow, they don't own.
fn spawn_poller<F>(
    label: String,
    period: Duration,
    conn: Arc<ConnectionInfo>,
    storage: Arc<Storage>,
    shutdown: CancellationToken,
    f: F,
) -> tokio::task::JoinHandle<()>
where
    F: Fn(Arc<ConnectionInfo>, Arc<Storage>) -> PollFut + Send + 'static,
{
    tokio::spawn(async move {
        let mut ticker = interval(period);
        // If a poll runs long we want to skip missed ticks, not burst.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        tracing::info!(target: "sentinel::scheduler", "poller {label} started (period={:?})", period);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!(target: "sentinel::scheduler", "poller {label} shutting down");
                    break;
                }
                _ = ticker.tick() => {
                    if let Err(e) = f(conn.clone(), storage.clone()).await {
                        tracing::warn!(target: "sentinel::scheduler", "poller {label} failed: {e:#}");
                    }
                }
            }
        }
    })
}
