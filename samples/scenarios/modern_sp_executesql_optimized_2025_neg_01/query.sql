-- False-positive guard: a plain static parameterized statement on SQL Server
-- 2025. There is no EXEC sp_executesql dynamic call, so the
-- OPTIMIZED_SP_EXECUTESQL advisory rule has nothing to attach to.
-- modern.sp_executesql_optimized_2025 must stay silent.
DECLARE @cid int = 42;

SELECT COUNT(*) AS OrderCount
FROM dbo.Orders
WHERE CustomerId = @cid;
