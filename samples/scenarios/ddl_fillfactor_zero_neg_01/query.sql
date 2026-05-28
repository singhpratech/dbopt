-- False-positive guard: a CREATE CLUSTERED INDEX whose leading key is a
-- sequential BIGINT IDENTITY (no GUID/_id/uid name hint). Inserts append at the
-- right edge of the B-tree, so there is no page-split risk and the missing
-- FILLFACTOR is harmless. ddl.fillfactor_default_zero_on_random_inserts must
-- stay silent because the key name is not GUID-like.
CREATE CLUSTERED INDEX CIX_Ledger_EntryNumber
    ON dbo.Ledger (EntryNumber);
