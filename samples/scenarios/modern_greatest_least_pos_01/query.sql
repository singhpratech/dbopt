-- Hand-rolled MAX-of-two-columns via CASE. On 2022+ GREATEST/LEAST are
-- native, optimizer-friendly, and tolerate NULLs more predictably.
SELECT  t.Id,
        CASE WHEN t.a > t.b THEN t.a ELSE t.b END AS HighWater,
        CASE WHEN t.a < t.b THEN t.a ELSE t.b END AS LowWater
FROM    dbo.Readings AS t;
