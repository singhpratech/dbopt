-- False-positive guard: a SELECT with ORDER BY that is bounded by OFFSET/FETCH
-- pagination. A bounded sort does not risk an unbounded tempdb spill, so
-- tempdb.spill_risk_large_sort must stay silent.
SELECT  o.OrderId,
        o.CustomerId,
        o.OrderDate
FROM    dbo.Orders AS o
WHERE   o.Status = 1
ORDER BY o.OrderDate DESC
OFFSET 100 ROWS FETCH NEXT 25 ROWS ONLY;

-- And a TOP-bounded ordered read — also safe.
SELECT TOP (50) p.ProductId, p.ListPrice
FROM   dbo.Products AS p
ORDER BY p.ListPrice DESC;
