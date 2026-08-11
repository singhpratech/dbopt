-- `[Merge]` is a column name, not the MERGE keyword. Reading it as one let an
-- identifier silence the only critical-severity rule for the rest of the batch.
SELECT RunId, [Merge] FROM dbo.RunFlags
UPDATE dbo.RunFlags SET Active = 0;
