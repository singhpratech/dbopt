-- Reading and dropping a global temp table that another session created is
-- not the design decision; only the creation site is.
INSERT INTO ##BlitzResults (Id) SELECT Id FROM #BlitzResults;
SELECT Id FROM ##BlitzResults;
IF OBJECT_ID('tempdb..##BlitzResults', 'U') IS NOT NULL DROP TABLE ##BlitzResults;
