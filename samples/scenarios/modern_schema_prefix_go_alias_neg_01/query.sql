-- `go` here is a table alias, not a batch separator. Treating it as one split
-- the batch mid-statement and un-suppressed the CTE reference after it.
WITH Recent AS (SELECT Id FROM dbo.Events)
SELECT go.Name, r.Id
FROM dbo.GeoOrigin AS go
JOIN Recent r ON r.Id = go.Id
WHERE r.Id > 5;
