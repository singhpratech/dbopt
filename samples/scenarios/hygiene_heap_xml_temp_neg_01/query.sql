-- A single XML column cannot be a clustered-index key at all, and the table is
-- a #temp the proc fills once and drops — the documented exception.
CREATE TABLE #x (x xml NOT NULL);
