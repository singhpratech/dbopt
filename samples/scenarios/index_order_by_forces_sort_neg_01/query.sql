-- ORDER BY on the same column the equality predicate already fixes: within a
-- single CustomerId the rows are already in order, so no Sort is introduced.
SELECT OrderId, Total FROM dbo.Orders WHERE CustomerId = 42 ORDER BY CustomerId;
