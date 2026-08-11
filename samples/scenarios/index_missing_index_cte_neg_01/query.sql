-- `ranked` is a CTE, not a table: you cannot CREATE INDEX on it, and `rn` is a
-- computed window value that exists nowhere on disk.
WITH ranked AS (
  SELECT OrderID, CustomerID,
         ROW_NUMBER() OVER (PARTITION BY CustomerID ORDER BY OrderDate DESC) AS rn
  FROM dbo.Orders
)
SELECT OrderID, CustomerID FROM ranked WHERE rn = 1;
