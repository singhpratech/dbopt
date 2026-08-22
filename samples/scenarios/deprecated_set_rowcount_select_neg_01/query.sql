-- SET ROWCOUNT limiting a SELECT is not the deprecated DML use.
SET ROWCOUNT 10
SELECT p.ProductName AS TenMostExpensiveProducts, p.UnitPrice
FROM dbo.Products AS p
ORDER BY p.UnitPrice DESC
SET ROWCOUNT 0
