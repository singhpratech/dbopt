-- "Rewrite as a UNION" does not exist for a DELETE or UPDATE: the OR chain is
-- simply the filter these statements need.
DELETE dbs
FROM #databases AS dbs
WHERE dbs.name LIKE 'x%'
   OR dbs.state = 1
   OR dbs.is_distributor = 1
   OR dbs.is_read_only = 1;

UPDATE el
SET el.category = 'trace'
FROM #error_log AS el
WHERE el.text LIKE 'DBCC TRACEON 3604%'
   OR el.text LIKE 'DBCC TRACEOFF 3604%'
   OR el.text LIKE 'DBCC TRACEON 3605%'
   OR el.text LIKE 'DBCC TRACEOFF 3605%';
