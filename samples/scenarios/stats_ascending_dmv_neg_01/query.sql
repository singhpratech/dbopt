-- A trailing-window filter on a DMV. DMVs have no statistics histogram to lag
-- behind inserts, so the ascending-key story cannot apply.
SELECT  h.snapshot_time, h.available_physical_memory_kb
FROM    sys.dm_os_memory_health_history AS h
WHERE   h.snapshot_time >= DATEADD(HOUR, -4, GETDATE());

SELECT  d.[filename], d.creation_time
FROM    [sys].[dm_server_memory_dumps] AS d
WHERE   d.[creation_time] >= DATEADD(YEAR, -1, GETDATE());
