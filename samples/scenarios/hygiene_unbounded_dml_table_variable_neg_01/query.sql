-- Clearing a table variable is the idiom, not an accident: it is session-scoped
-- and holds only what this batch put in it.
DELETE FROM @DirectoryInfo;
DELETE FROM #StagedRows;
