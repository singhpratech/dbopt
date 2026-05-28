-- Five-minute time-bucket built the old way: floor the minute difference from a
-- fixed origin and add it back. DATE_BUCKET (2022+) expresses this directly and
-- supports arbitrary origins.
SELECT
    DATEADD(MINUTE, (DATEDIFF(MINUTE, '2020-01-01', EventTime) / 5) * 5, '2020-01-01') AS Bucket,
    COUNT(*) AS Hits
FROM dbo.Telemetry
GROUP BY DATEADD(MINUTE, (DATEDIFF(MINUTE, '2020-01-01', EventTime) / 5) * 5, '2020-01-01');
