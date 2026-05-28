-- False-positive guard: a fixed, literal BETWEEN date range. This is NOT a
-- trailing now()-relative window (no DATEADD over GETDATE()/SYSDATETIME() with a
-- negative offset), so the ascending-key hotspot rule must stay silent.
SELECT s.SaleId, s.Amount, s.SaleDate
FROM   dbo.Sales AS s
WHERE  s.SaleDate BETWEEN '2025-01-01' AND '2025-03-31';
