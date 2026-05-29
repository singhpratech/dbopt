-- Per-deadlock-graph hash so we store ONE row per real deadlock event and dedup
-- across polls. Previously the poller stored the entire system_health ring
-- buffer as one row per snapshot, so COUNT(*) counted snapshots, not deadlocks
-- (a false "N deadlocks" alarm). Old snapshot rows have graph_hash IS NULL and
-- are purged at upgrade.
ALTER TABLE deadlock_capture ADD COLUMN graph_hash TEXT;
