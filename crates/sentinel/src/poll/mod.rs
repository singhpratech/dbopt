//! Per-surface pollers. Each submodule exposes a single async `poll_*`
//! function that the scheduler invokes on its cadence. Bodies are stubs
//! today — next session swaps in the real `tiberius` queries.

pub mod cpu_pressure;
pub mod deadlocks;
pub mod index_usage;
pub mod io_latency;
pub mod live;
pub mod memory_headroom;
pub mod plan_cache;
pub mod query_store;
pub mod sizes;
pub mod tempdb_contention;
pub mod waits;
