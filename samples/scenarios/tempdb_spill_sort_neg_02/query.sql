-- Bounded sorts in every TOP spelling: bare literal, @variable, WITH TIES,
-- and a scalar subquery with its own TOP 1 ORDER BY. None of these is an
-- unbounded sort, so tempdb.spill_risk_large_sort must stay silent.
SELECT TOP 10 p.ProductId, p.ListPrice
FROM   dbo.Products AS p
ORDER BY p.ListPrice DESC;

DECLARE @n INT = 25;
SELECT TOP @n o.OrderId
FROM   dbo.Orders AS o
ORDER BY o.OrderDate DESC;

SELECT TOP 5 WITH TIES o.OrderId, o.TotalCents
FROM   dbo.Orders AS o
ORDER BY o.TotalCents DESC;

SELECT ct.ClaimId
FROM   dbo.claims_transactions AS ct
WHERE  ct.ProviderId = (SELECT TOP 1 Id FROM dbo.providers ORDER BY Id);
