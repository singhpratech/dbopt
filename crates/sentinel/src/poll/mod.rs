//! Per-surface pollers. Each submodule exposes a single async `poll_*`
//! function that the scheduler invokes on its cadence. Each one runs a real
//! read-only `tiberius` query against the live server's DMVs, maps the result
//! into the matching `storage` row struct, and persists it (the cumulative
//! surfaces — waits, index usage, file I/O — diff against the prior snapshot
//! held in `poller_state` to emit per-window deltas). Pollers degrade
//! gracefully: a missing DMV or a lack of VIEW SERVER STATE logs once and skips
//! the tick rather than failing the whole daemon.

pub mod alert_eval;
pub mod cpu_pressure;
pub mod deadlocks;
pub mod index_usage;
pub mod io_latency;
pub mod live;
pub mod memory_headroom;
pub mod missing_index;
pub mod plan_cache;
pub mod query_store;
pub mod sizes;
pub mod tempdb_contention;
pub mod waits;
