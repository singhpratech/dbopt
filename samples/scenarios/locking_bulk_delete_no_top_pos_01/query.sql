-- Six-month audit purge in a single statement. With no TOP and no supporting
-- index on CreatedAt the engine takes an X lock on a huge range, blocks
-- writers, and is a prime candidate for lock escalation to the whole table.
-- Should be batched: `DELETE TOP (5000) ... ; WHILE @@ROWCOUNT > 0 ...`.
DELETE FROM dbo.Audit
WHERE  CreatedAt < DATEADD(MONTH, -6, GETDATE());
