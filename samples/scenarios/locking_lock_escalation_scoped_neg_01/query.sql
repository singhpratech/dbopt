-- False-positive guard: lock escalation is disabled per-table via the supported
-- ALTER TABLE ... SET (LOCK_ESCALATION = DISABLE) syntax, scoped to one table --
-- not the server-wide trace flags 1211/1224. There is no DBCC TRACEON and no
-- -T startup flag, so locking.lock_escalation_disabled_globally must stay silent.
ALTER TABLE dbo.HotQueue
    SET (LOCK_ESCALATION = DISABLE);
