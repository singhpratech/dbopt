-- SET ROWCOUNT 10 on the preceding line bounds this SELECT exactly like TOP 10
-- would; the "without TOP" claim is false here.
SET ROWCOUNT 10
SELECT p.ProductName AS TenMostExpensiveProducts, p.UnitPrice
FROM Products AS p
ORDER BY p.UnitPrice DESC
SET ROWCOUNT 0
