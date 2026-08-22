-- A #temp table is created and dropped inside one session of one proc; the
-- "schema differs between callers" hazard does not apply.
CREATE TABLE #BlitzResults (
    CheckID  int,
    Priority tinyint,
    Finding  nvarchar(200)
);
