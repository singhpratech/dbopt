-- Session-wide read-uncommitted: a `WITH (NOLOCK)` spray painted across
-- everything in the connection. Allocation-order scans, duplicate rows,
-- missed rows, dirty reads — the whole zoo. Should be replaced with RCSI
-- or accepted only for ad-hoc DBA diagnostics.
SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED;

SELECT  o.OrderId,
        o.CustomerId,
        o.TotalCents
FROM    dbo.Orders AS o
WHERE   o.OrderDate >= DATEADD(DAY, -1, GETDATE());
