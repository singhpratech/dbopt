-- An aggregating scan over a DMV. A columnstore index cannot be created on
-- sys.dm_os_memory_health_history, so the advice has nowhere to go.
SELECT  h.snapshot_time,
        SUM(h.available_physical_memory_kb) AS avail_kb,
        COUNT(*)                            AS samples
FROM    sys.dm_os_memory_health_history AS h
GROUP BY h.snapshot_time;
