-- Nightly maintenance job: full-scan stats refresh on a billion-row fact table.
-- FULLSCAN on a table this size hammers I/O and tempdb, often for marginal
-- accuracy gain over a well-sized SAMPLE. Better: PERSIST_SAMPLE_PERCENT or
-- INCREMENTAL stats with targeted partition rebuilds.
UPDATE STATISTICS dbo.Orders WITH FULLSCAN;
