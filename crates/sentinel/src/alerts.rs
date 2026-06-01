//! Threshold alerting engine.
//!
//! Sentinel was purely passive: it captured time-series telemetry and waited
//! for someone to open a report. This module makes it *active* — after a poll
//! persists a vitals sample, the configured [`AlertRule`]s are evaluated against
//! the just-captured values; a breach fires a [`FiredAlert`] that is persisted
//! (de-duped so the same breach can't spam every tick) and pushed to a webhook.
//!
//! Design constraints (honesty discipline):
//!   * Every default threshold traces to a cited source (see [`default_rules`]).
//!     Where the industry guideline is a derived floor (PLE), the rule carries a
//!     dynamic formula and we evaluate the *measured* value against it.
//!   * A metric we couldn't read produces NO alert (a missing value, never a
//!     guessed one) — mirroring the "report MEASURED values" rule in the engine.
//!   * The evaluator is a pure function so the comparator/boundary logic is unit
//!     tested in isolation.
//!
//! The metrics map onto fields the pollers ALREADY persist (see
//! [`MetricSnapshot`]); we never invent a new collection surface here.

use serde::{Deserialize, Serialize};

/// How a measured value is compared against a rule's threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Comparator {
    /// measured > threshold
    Gt,
    /// measured >= threshold
    Ge,
    /// measured < threshold
    Lt,
    /// measured <= threshold
    Le,
}

impl Comparator {
    /// Pure comparison: does `measured <cmp> threshold` hold?
    pub fn holds(self, measured: f64, threshold: f64) -> bool {
        match self {
            Comparator::Gt => measured > threshold,
            Comparator::Ge => measured >= threshold,
            Comparator::Lt => measured < threshold,
            Comparator::Le => measured <= threshold,
        }
    }

    /// Human glyph for messages ("PLE 412 < floor 4800").
    pub fn glyph(self) -> &'static str {
        match self {
            Comparator::Gt => ">",
            Comparator::Ge => ">=",
            Comparator::Lt => "<",
            Comparator::Le => "<=",
        }
    }
}

/// Severity tier. Drives the webhook colour field and the UI tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    /// Webhook colour hint (red = Critical / amber = Warning / grey = Info).
    /// Used directly by the generic webhook body and by the Teams `themeColor`.
    pub fn color(self) -> &'static str {
        match self {
            Severity::Info => "#6b7280",     // grey
            Severity::Warning => "#f59e0b",  // amber
            Severity::Critical => "#dc2626", // red
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
}

/// How a rule's threshold is computed. Most rules are a fixed number, but PLE's
/// industry guideline is a *derived* floor (buffer-pool-GB / 4 * 300s), so it
/// can't be a constant — it depends on a value captured in the same sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Threshold {
    /// A fixed numeric threshold.
    Fixed { value: f64 },
    /// Page-Life-Expectancy floor = (buffer_pool_GB / 4) * 300 seconds
    /// (Kehayias). The buffer-pool size comes from the same memory sample, so we
    /// compute the floor at evaluation time. `min_floor` clamps the floor up so
    /// tiny dev boxes still get a sane minimum (300s — the legacy static rule
    /// becomes the lower bound, never the upper).
    PleFloorPer4Gb { min_floor: f64 },
}

/// One configurable alert rule. `id` is stable (matches the SPEC catalogue) so
/// de-dup and config round-trips key on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRule {
    pub id: String,
    /// Human label for the metric being watched (our own vocabulary).
    pub metric: String,
    pub comparator: Comparator,
    pub threshold: Threshold,
    pub severity: Severity,
    /// Whether this rule is armed. Off rules are kept in config so the user can
    /// re-enable without re-entering the threshold.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Short source citation for the threshold (shown in the UI / payload).
    #[serde(default)]
    pub source: String,
}

fn default_true() -> bool {
    true
}

/// A breach: the rule fired against a concrete measured value at a concrete
/// computed threshold. `message` is a one-line human summary used as the webhook
/// `text` and the feed row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FiredAlert {
    pub rule_id: String,
    pub metric: String,
    pub value: f64,
    pub threshold: f64,
    pub severity: Severity,
    pub message: String,
}

/// The set of metric values extracted from a freshly-persisted vitals sample.
/// Every field is `Option` so a metric we couldn't read produces NO alert
/// (a missing reading, never a zero we'd misjudge as "healthy").
#[derive(Debug, Clone, Default)]
pub struct MetricSnapshot {
    /// avg(runnable_tasks_count) across VISIBLE ONLINE schedulers.
    pub cpu_runnable_tasks_avg: Option<f64>,
    /// SUM(signal_wait_time_ms)/SUM(wait_time_ms) over the window, as a percent.
    pub cpu_signal_wait_pct: Option<f64>,
    /// Buffer Manager Page Life Expectancy (seconds).
    pub memory_ple_secs: Option<f64>,
    /// Buffer pool size in GB (Total Server Memory), used to derive the PLE floor.
    pub buffer_pool_gb: Option<f64>,
    /// RESOURCE_SEMAPHORE waiter_count (pending workspace-memory grants).
    pub pending_memory_grants: Option<f64>,
    /// single-use ad-hoc plan bytes / total plan-cache bytes, as a percent.
    pub plancache_singleuse_pct: Option<f64>,
    /// COUNT of PAGELATCH waiters on tempdb allocation pages.
    pub tempdb_pagelatch_waiters: Option<f64>,
    /// Worst per-file DATA latency this window (ms).
    pub io_data_latency_ms: Option<f64>,
    /// Worst per-file LOG WRITE latency this window (ms).
    pub io_log_write_latency_ms: Option<f64>,
    /// New deadlock graphs captured this window.
    pub deadlocks: Option<f64>,
    /// Blocked sessions observed right now.
    pub blocked_sessions: Option<f64>,
}

impl MetricSnapshot {
    /// Resolve a metric id to its measured value, if present.
    fn value_for(&self, id: &str) -> Option<f64> {
        match id {
            "cpu.runnable_tasks_high" => self.cpu_runnable_tasks_avg,
            "cpu.signal_wait_pct_high" => self.cpu_signal_wait_pct,
            "memory.ple_below_floor" => self.memory_ple_secs,
            "memory.pending_memory_grants" => self.pending_memory_grants,
            "plancache.adhoc_singleuse_pct" => self.plancache_singleuse_pct,
            "tempdb.pagelatch_contention" => self.tempdb_pagelatch_waiters,
            "io.data_file_latency_ms" => self.io_data_latency_ms,
            "io.log_write_latency_ms" => self.io_log_write_latency_ms,
            "reliability.deadlocks" => self.deadlocks,
            "reliability.blocking" => self.blocked_sessions,
            _ => None,
        }
    }
}

/// Resolve a rule's threshold to a concrete number given the current sample.
/// Returns `None` when the threshold needs a value we couldn't read (e.g. the
/// PLE floor needs the buffer-pool size) — in which case the rule does not fire.
fn resolve_threshold(rule: &AlertRule, snap: &MetricSnapshot) -> Option<f64> {
    match &rule.threshold {
        Threshold::Fixed { value } => Some(*value),
        Threshold::PleFloorPer4Gb { min_floor } => {
            let gb = snap.buffer_pool_gb?;
            // floor = (GB / 4) * 300, clamped up to min_floor so a 2GB dev box
            // still alerts below a sane minimum rather than ~150s.
            let floor = (gb / 4.0) * 300.0;
            Some(floor.max(*min_floor))
        }
    }
}

/// PURE evaluation: does this rule breach against the measured sample?
///
/// Returns `Some(FiredAlert)` only when (a) the rule is enabled, (b) the metric
/// was actually measured in this sample, (c) the (possibly-derived) threshold
/// could be resolved, and (d) the comparator holds. Otherwise `None` — a metric
/// we couldn't read never fires.
pub fn evaluate_alert(rule: &AlertRule, snap: &MetricSnapshot) -> Option<FiredAlert> {
    if !rule.enabled {
        return None;
    }
    let measured = snap.value_for(&rule.id)?;
    let threshold = resolve_threshold(rule, snap)?;
    if !rule.comparator.holds(measured, threshold) {
        return None;
    }
    let message = format!(
        "{}: {} {} {}",
        rule.metric,
        fmt_num(measured),
        rule.comparator.glyph(),
        fmt_num(threshold),
    );
    Some(FiredAlert {
        rule_id: rule.id.clone(),
        metric: rule.metric.clone(),
        value: measured,
        threshold,
        severity: rule.severity,
        message,
    })
}

/// Evaluate every rule against the sample, returning all breaches (callers
/// de-dupe against already-firing state before persisting/notifying).
pub fn evaluate_all(rules: &[AlertRule], snap: &MetricSnapshot) -> Vec<FiredAlert> {
    rules.iter().filter_map(|r| evaluate_alert(r, snap)).collect()
}

/// Trim a float for human display: integers print clean, fractions keep 1dp.
fn fmt_num(n: f64) -> String {
    if (n - n.round()).abs() < 1e-9 {
        format!("{}", n.round() as i64)
    } else {
        format!("{n:.1}")
    }
}

/// The full alerting configuration: an optional notification webhook plus the
/// armed rules. Persisted inside `SentinelConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertConfig {
    /// Generic POST target (works for raw webhook, Slack incoming-webhook, Teams
    /// incoming-webhook). `None`/empty = persist alerts but don't notify.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Webhook body flavour so we emit the right JSON shape for the target.
    #[serde(default)]
    pub webhook_format: WebhookFormat,
    /// Cooldown before an already-firing rule re-fires (seconds). Suppresses
    /// per-tick spam: we only (re)fire when state changes OR the cooldown lapses.
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    pub rules: Vec<AlertRule>,
}

/// Webhook body flavour. All three are "a generic POST of application/json";
/// they differ only in the JSON shape the receiver expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebhookFormat {
    /// `{ "text": "...", "severity": "...", "color": "...", "alert": {...} }`.
    #[default]
    Generic,
    /// Slack incoming-webhook: `{ "text": "..." }` (Block Kit optional).
    Slack,
    /// Teams incoming-webhook MessageCard with severity-driven `themeColor`.
    Teams,
}

pub fn default_cooldown_secs() -> u64 {
    900
} // 15 min: don't re-page the same standing condition every poll.

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            webhook_url: None,
            webhook_format: WebhookFormat::Generic,
            cooldown_secs: default_cooldown_secs(),
            rules: default_rules(),
        }
    }
}

/// The grounded default rule set. Every threshold below traces to the cited
/// source in the project SPEC. We ship the *warning* tier of each tiered metric
/// as the armed default; the higher tier is encoded for metrics that have a
/// single clear escalation (IO data/log) by shipping a second Critical rule.
pub fn default_rules() -> Vec<AlertRule> {
    vec![
        // CPU pressure — runnable tasks queued for a CPU. Medium tier = 10.
        // sqlmonitormetrics.red-gate.com/average-runnable-task-count/
        AlertRule {
            id: "cpu.runnable_tasks_high".into(),
            metric: "Runnable tasks waiting for a CPU".into(),
            comparator: Comparator::Ge,
            threshold: Threshold::Fixed { value: 10.0 },
            severity: Severity::Warning,
            enabled: true,
            source: "Red Gate runnable-task tiers (Med=10/High=20)".into(),
        },
        // CPU pressure corroboration — signal-wait % of total resource waits.
        // sqlshack.com boost-sql-server-performance-with-wait-statistics
        AlertRule {
            id: "cpu.signal_wait_pct_high".into(),
            metric: "Signal-wait share of total waits".into(),
            comparator: Comparator::Gt,
            threshold: Threshold::Fixed { value: 35.0 },
            severity: Severity::Warning,
            enabled: true,
            source: "SQLShack wait-stats: signal-wait > 35% = CPU pressure".into(),
        },
        // Memory — PLE below the per-4GB floor (Kehayias). Dynamic threshold.
        // sqlperformance.com knee-jerk-page-life-expectancy
        AlertRule {
            id: "memory.ple_below_floor".into(),
            metric: "Cache retention (PLE) below floor".into(),
            comparator: Comparator::Lt,
            threshold: Threshold::PleFloorPer4Gb { min_floor: 300.0 },
            severity: Severity::Warning,
            enabled: true,
            source: "Kehayias PLE floor = (buffer GB / 4) * 300s".into(),
        },
        // Memory — queries queued for a workspace-memory grant. Any sustained
        // waiter blocks execution; engine errors 8645 after ~20 min.
        // learn.microsoft.com troubleshoot-memory-grant-issues
        AlertRule {
            id: "memory.pending_memory_grants".into(),
            metric: "Queries waiting for a memory grant".into(),
            comparator: Comparator::Ge,
            threshold: Threshold::Fixed { value: 1.0 },
            severity: Severity::Warning,
            enabled: true,
            source: "RESOURCE_SEMAPHORE waiter >= 1 (timeout error 8645 ~20m)".into(),
        },
        // Plan cache — single-use ad-hoc plan share. Warning tier = 20%.
        // blog.sqlauthority.com optimize-ad-hoc-workloads
        AlertRule {
            id: "plancache.adhoc_singleuse_pct".into(),
            metric: "Single-use ad-hoc plan share of cache".into(),
            comparator: Comparator::Ge,
            threshold: Threshold::Fixed { value: 20.0 },
            severity: Severity::Warning,
            enabled: true,
            source: "Industry action range 15-30%; warn at 20%".into(),
        },
        // tempdb — PAGELATCH waiters on allocation bitmaps. Medium tier = 10.
        // sqlmonitormetrics.red-gate.com/tempdb-allocation-contention/
        AlertRule {
            id: "tempdb.pagelatch_contention".into(),
            metric: "tempdb allocation-page waiters".into(),
            comparator: Comparator::Ge,
            threshold: Threshold::Fixed { value: 10.0 },
            severity: Severity::Warning,
            enabled: true,
            source: "Red Gate tempdb tiers (Med=10/High=50)".into(),
        },
        // Storage — data-file latency. High band >20ms (Warning), Critical >100ms.
        // sqlperformance.com monitoring-read-write-latency (SQLskills bands)
        AlertRule {
            id: "io.data_file_latency_ms".into(),
            metric: "Data-file I/O latency".into(),
            comparator: Comparator::Gt,
            threshold: Threshold::Fixed { value: 20.0 },
            severity: Severity::Warning,
            enabled: true,
            source: "SQLskills bands: 20-100ms bad".into(),
        },
        AlertRule {
            id: "io.data_file_latency_ms".into(),
            metric: "Data-file I/O latency (critical)".into(),
            comparator: Comparator::Gt,
            threshold: Threshold::Fixed { value: 100.0 },
            severity: Severity::Critical,
            enabled: true,
            source: "SQLskills bands: >100ms critical".into(),
        },
        // Storage — transaction-log WRITE latency. Target <=1ms (SQLCAT), so
        // >1ms Warning, >5ms High.
        AlertRule {
            id: "io.log_write_latency_ms".into(),
            metric: "Transaction-log write latency".into(),
            comparator: Comparator::Gt,
            threshold: Threshold::Fixed { value: 5.0 },
            severity: Severity::Warning,
            enabled: true,
            source: "SQLCAT log-write target <=1ms; high band >5ms".into(),
        },
        // Reliability — any new deadlock graph captured this window.
        AlertRule {
            id: "reliability.deadlocks".into(),
            metric: "Deadlocks captured".into(),
            comparator: Comparator::Ge,
            threshold: Threshold::Fixed { value: 1.0 },
            severity: Severity::Critical,
            enabled: true,
            source: "Any new deadlock graph (system_health ring buffer)".into(),
        },
        // Reliability — blocked sessions observed right now.
        AlertRule {
            id: "reliability.blocking".into(),
            metric: "Blocked sessions".into(),
            comparator: Comparator::Ge,
            threshold: Threshold::Fixed { value: 1.0 },
            severity: Severity::Warning,
            enabled: true,
            source: "Any session blocked on another (live request snapshot)".into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, cmp: Comparator, t: f64, sev: Severity) -> AlertRule {
        AlertRule {
            id: id.into(),
            metric: id.into(),
            comparator: cmp,
            threshold: Threshold::Fixed { value: t },
            severity: sev,
            enabled: true,
            source: String::new(),
        }
    }

    #[test]
    fn comparator_holds_each_direction() {
        assert!(Comparator::Gt.holds(11.0, 10.0));
        assert!(!Comparator::Gt.holds(10.0, 10.0)); // strict
        assert!(Comparator::Ge.holds(10.0, 10.0)); // boundary fires
        assert!(!Comparator::Ge.holds(9.99, 10.0));
        assert!(Comparator::Lt.holds(299.0, 300.0));
        assert!(!Comparator::Lt.holds(300.0, 300.0)); // strict
        assert!(Comparator::Le.holds(300.0, 300.0)); // boundary fires
        assert!(!Comparator::Le.holds(300.1, 300.0));
    }

    #[test]
    fn boundary_at_threshold_ge_fires_gt_does_not() {
        // runnable-tasks rule: Ge 10 → exactly 10 must fire.
        let r = rule("cpu.runnable_tasks_high", Comparator::Ge, 10.0, Severity::Warning);
        let snap = MetricSnapshot {
            cpu_runnable_tasks_avg: Some(10.0),
            ..Default::default()
        };
        let fired = evaluate_alert(&r, &snap).expect("Ge boundary fires");
        assert_eq!(fired.value, 10.0);
        assert_eq!(fired.threshold, 10.0);
        assert_eq!(fired.severity, Severity::Warning);

        // 9 must NOT fire.
        let below = MetricSnapshot {
            cpu_runnable_tasks_avg: Some(9.0),
            ..Default::default()
        };
        assert!(evaluate_alert(&r, &below).is_none());
    }

    #[test]
    fn missing_metric_never_fires() {
        // The rule is armed but the sample didn't read this metric → no alert,
        // never a guessed zero.
        let r = rule("cpu.runnable_tasks_high", Comparator::Ge, 10.0, Severity::Warning);
        let snap = MetricSnapshot::default(); // everything None
        assert!(evaluate_alert(&r, &snap).is_none());
    }

    #[test]
    fn disabled_rule_never_fires() {
        let mut r = rule("tempdb.pagelatch_contention", Comparator::Ge, 10.0, Severity::Warning);
        r.enabled = false;
        let snap = MetricSnapshot {
            tempdb_pagelatch_waiters: Some(50.0),
            ..Default::default()
        };
        assert!(evaluate_alert(&r, &snap).is_none());
    }

    #[test]
    fn ple_dynamic_floor_uses_buffer_pool_size() {
        // 64GB buffer pool → floor = (64/4)*300 = 4800s. PLE 4000 < 4800 fires.
        let r = AlertRule {
            id: "memory.ple_below_floor".into(),
            metric: "PLE".into(),
            comparator: Comparator::Lt,
            threshold: Threshold::PleFloorPer4Gb { min_floor: 300.0 },
            severity: Severity::Warning,
            enabled: true,
            source: String::new(),
        };
        let snap = MetricSnapshot {
            memory_ple_secs: Some(4000.0),
            buffer_pool_gb: Some(64.0),
            ..Default::default()
        };
        let fired = evaluate_alert(&r, &snap).expect("PLE below 4800 floor fires");
        assert_eq!(fired.threshold, 4800.0);

        // PLE 5000 > 4800 floor → healthy, no fire.
        let healthy = MetricSnapshot {
            memory_ple_secs: Some(5000.0),
            buffer_pool_gb: Some(64.0),
            ..Default::default()
        };
        assert!(evaluate_alert(&r, &healthy).is_none());
    }

    #[test]
    fn ple_floor_clamps_to_min_on_tiny_box() {
        // 2GB box → raw floor = (2/4)*300 = 150s, clamped UP to 300.
        let r = AlertRule {
            id: "memory.ple_below_floor".into(),
            metric: "PLE".into(),
            comparator: Comparator::Lt,
            threshold: Threshold::PleFloorPer4Gb { min_floor: 300.0 },
            severity: Severity::Warning,
            enabled: true,
            source: String::new(),
        };
        // PLE 200 < clamped floor 300 → fires.
        let snap = MetricSnapshot {
            memory_ple_secs: Some(200.0),
            buffer_pool_gb: Some(2.0),
            ..Default::default()
        };
        let fired = evaluate_alert(&r, &snap).expect("clamped floor fires");
        assert_eq!(fired.threshold, 300.0);
    }

    #[test]
    fn ple_without_buffer_pool_size_does_not_fire() {
        // Can't resolve the dynamic floor → no alert (don't guess a floor).
        let r = AlertRule {
            id: "memory.ple_below_floor".into(),
            metric: "PLE".into(),
            comparator: Comparator::Lt,
            threshold: Threshold::PleFloorPer4Gb { min_floor: 300.0 },
            severity: Severity::Warning,
            enabled: true,
            source: String::new(),
        };
        let snap = MetricSnapshot {
            memory_ple_secs: Some(100.0),
            buffer_pool_gb: None,
            ..Default::default()
        };
        assert!(evaluate_alert(&r, &snap).is_none());
    }

    #[test]
    fn evaluate_all_returns_every_breach() {
        let rules = default_rules();
        // A sample that trips CPU runnable-tasks + tempdb + data-file IO critical.
        let snap = MetricSnapshot {
            cpu_runnable_tasks_avg: Some(25.0),
            tempdb_pagelatch_waiters: Some(60.0),
            io_data_latency_ms: Some(150.0), // trips BOTH the 20ms warning and 100ms critical
            ..Default::default()
        };
        let fired = evaluate_all(&rules, &snap);
        let ids: Vec<&str> = fired.iter().map(|f| f.rule_id.as_str()).collect();
        assert!(ids.contains(&"cpu.runnable_tasks_high"));
        assert!(ids.contains(&"tempdb.pagelatch_contention"));
        // io.data_file_latency_ms appears twice (warning + critical tiers).
        let io_count = fired.iter().filter(|f| f.rule_id == "io.data_file_latency_ms").count();
        assert_eq!(io_count, 2);
    }

    #[test]
    fn default_config_ships_spec_rules() {
        let cfg = AlertConfig::default();
        assert!(cfg.rules.iter().any(|r| r.id == "cpu.runnable_tasks_high"));
        assert!(cfg.rules.iter().any(|r| r.id == "memory.ple_below_floor"));
        assert!(cfg.rules.iter().any(|r| r.id == "io.log_write_latency_ms"));
        // Every default rule carries a source citation (honesty discipline).
        assert!(cfg.rules.iter().all(|r| !r.source.is_empty()));
    }

    #[test]
    fn message_formats_cleanly() {
        let r = rule("plancache.adhoc_singleuse_pct", Comparator::Ge, 20.0, Severity::Warning);
        let snap = MetricSnapshot {
            plancache_singleuse_pct: Some(33.5),
            ..Default::default()
        };
        let fired = evaluate_alert(&r, &snap).unwrap();
        // integers print clean, the fraction keeps 1dp
        assert!(fired.message.contains("33.5"));
        assert!(fired.message.contains(">="));
        assert!(fired.message.contains("20"));
    }
}
