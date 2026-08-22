-- Upper bound only: `StallRank <= 20` is a top-N filter, not a page slice.
WITH readstats AS (
    SELECT ROW_NUMBER() OVER (ORDER BY wd2.avg_stall_read_ms DESC) AS StallRank,
           wd1.DatabaseName, wd2.avg_stall_read_ms
    FROM #FileStats AS wd2
    JOIN #FileStats AS wd1 ON wd2.FileID = wd1.FileID AND wd2.SampleTime > wd1.SampleTime
)
SELECT DatabaseName, avg_stall_read_ms
FROM readstats
WHERE StallRank <= 20
ORDER BY StallRank;
