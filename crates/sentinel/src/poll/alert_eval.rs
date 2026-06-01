//! Alert-evaluation pass.
//!
//! This is the bridge that makes Sentinel ACTIVE. The vitals pollers persist
//! their samples independently; this pass runs on its own cadence, reads back
//! the most-recent persisted values for one instance, assembles a
//! [`MetricSnapshot`], evaluates the configured [`AlertRule`]s against it, and
//! for every NEW breach (de-duped by `should_fire_alert`) persists a row and
//! best-effort POSTs it to the configured webhook.
//!
//! No live DB connection is needed — every metric maps to data the other
//! pollers already captured. A metric a poller couldn't read (missing DMV /
//! permission) is simply absent from the snapshot, so its rule never fires.

use chrono::Utc;

use crate::alerts::{evaluate_all, AlertConfig, MetricSnapshot};
use crate::notify::notify_webhook;
use crate::storage::Storage;

/// Build the metric snapshot for `instance_id` from the latest persisted
/// telemetry. Each field is best-effort: absent telemetry → `None` → no alert.
fn build_snapshot(storage: &Storage, instance_id: i64) -> MetricSnapshot {
    let mut snap = MetricSnapshot::default();

    if let Some(cpu) = storage.latest_cpu_pressure(instance_id) {
        // avg(runnable_tasks) across online schedulers — the SPEC metric.
        if cpu.online_schedulers > 0 {
            snap.cpu_runnable_tasks_avg =
                Some(cpu.runnable_tasks as f64 / cpu.online_schedulers as f64);
        }
    }

    snap.cpu_signal_wait_pct = storage.latest_signal_wait_pct(instance_id);

    if let Some(mem) = storage.latest_memory_headroom(instance_id) {
        snap.memory_ple_secs = Some(mem.page_life_expectancy as f64);
        snap.pending_memory_grants = Some(mem.pending_memory_grants as f64);
        // Buffer-pool GB drives the dynamic PLE floor. Total Server Memory is in
        // KB; only report it when it's a real reading (> 0).
        if mem.total_server_memory_kb > 0 {
            snap.buffer_pool_gb = Some(mem.total_server_memory_kb as f64 / 1_048_576.0);
        }
    }

    if let Some(pc) = storage.latest_plan_cache(instance_id) {
        // Single-use share BY SIZE (bytes), per the SPEC metric definition.
        if pc.total_size_kb > 0 {
            snap.plancache_singleuse_pct =
                Some((pc.single_use_size_kb as f64 / pc.total_size_kb as f64) * 100.0);
        }
    }

    if let Some(t) = storage.latest_tempdb_contention(instance_id) {
        snap.tempdb_pagelatch_waiters = Some(t.pagelatch_waiters as f64);
    }

    // Worst per-file latency this window, split data vs log. The IO poller labels
    // each file's type ("ROWS" = data, "LOG" = transaction log).
    let io = storage.latest_io_latency(instance_id);
    let mut worst_data: Option<f64> = None;
    let mut worst_log_write: Option<f64> = None;
    for f in &io {
        let is_log = f.file_type.eq_ignore_ascii_case("LOG");
        if is_log {
            // The log is written every commit; the SPEC band is on WRITE latency.
            let v = f.avg_write_latency_ms;
            worst_log_write = Some(worst_log_write.map_or(v, |w: f64| w.max(v)));
        } else {
            let v = f.avg_read_latency_ms.max(f.avg_write_latency_ms);
            worst_data = Some(worst_data.map_or(v, |w: f64| w.max(v)));
        }
    }
    snap.io_data_latency_ms = worst_data;
    snap.io_log_write_latency_ms = worst_log_write;

    // Reliability: deadlocks in the last ~5 minutes + blocked sessions now.
    let since = Utc::now() - chrono::Duration::minutes(5);
    let dl = storage.deadlocks_since(instance_id, since);
    snap.deadlocks = Some(dl as f64);
    snap.blocked_sessions = storage.latest_blocked_sessions(instance_id).map(|n| n as f64);

    snap
}

/// Run one alert-evaluation pass for `instance_name`. Persists every NEW breach
/// and fires the webhook for it. Never errors out the scheduler — every failure
/// path logs and returns `Ok(())`.
pub async fn evaluate_instance(
    storage: &Storage,
    instance_name: &str,
    config: &AlertConfig,
) -> anyhow::Result<()> {
    let Some(instance_id) = storage.get_instance_id_by_name(instance_name) else {
        // No instance row yet (nothing captured) → nothing to evaluate.
        return Ok(());
    };

    let snap = build_snapshot(storage, instance_id);
    let breaches = evaluate_all(&config.rules, &snap);
    if breaches.is_empty() {
        return Ok(());
    }

    let now = Utc::now();
    for alert in breaches {
        // De-dup: skip a standing condition that's still inside its cooldown.
        if !storage.should_fire_alert(instance_id, &alert.rule_id, now, config.cooldown_secs) {
            continue;
        }
        let id = match storage.insert_fired_alert(
            instance_id,
            now,
            &alert.rule_id,
            &alert.metric,
            alert.value,
            alert.threshold,
            alert.severity.as_str(),
            &alert.message,
        ) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(target: "sentinel::alerts", "persist fired alert failed: {e:#}");
                continue;
            }
        };
        tracing::info!(
            target: "sentinel::alerts",
            "ALERT fired on {instance_name}: {} ({})",
            alert.message, alert.severity.as_str()
        );

        // Best-effort notification — a webhook failure never crashes the poller.
        if let Some(url) = config.webhook_url.as_deref() {
            if !url.trim().is_empty()
                && notify_webhook(url, &alert, instance_name, config.webhook_format).await
            {
                if let Err(e) = storage.mark_alert_notified(id) {
                    tracing::warn!(target: "sentinel::alerts", "mark notified failed: {e:#}");
                }
            }
        }
    }
    Ok(())
}
