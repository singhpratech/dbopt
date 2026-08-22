-- A clustered index built on a #temp table after a cursor finished loading it.
-- session_id is a SMALLINT and the table is filled once; no random inserts.
CREATE TABLE #locks (
    session_id    smallint NOT NULL,
    request_id    int      NOT NULL,
    database_name sysname  NOT NULL
);
CREATE CLUSTERED INDEX IX_SRD ON #locks (session_id, request_id, database_name);
