-- CTE names declared with a column list, and with a comment between the name
-- and its body, are not tables and cannot be schema-qualified.
WITH BackupPaths (StartPosition, EndPosition, PathItem) AS (
    SELECT 1, CHARINDEX(',', @paths + ','), NULL
    UNION ALL
    SELECT EndPosition + 1, CHARINDEX(',', @paths + ',', EndPosition + 1),
           SUBSTRING(@paths, EndPosition + 1, 10)
    FROM BackupPaths
    WHERE EndPosition < LEN(@paths) + 1
)
INSERT INTO @PathItem
SELECT PathItem FROM BackupPaths;

WITH
    /* walk the chain */
    blockers
(
    session_id, blocking_session_id
) AS
(
    SELECT sp.spid, sp.blocked FROM sys.sysprocesses AS sp WHERE sp.blocked = 0
    UNION ALL
    SELECT sp.spid, sp.blocked
    FROM blockers AS b
    JOIN sys.sysprocesses AS sp ON sp.blocked = b.session_id
)
SELECT * FROM blockers AS b;
