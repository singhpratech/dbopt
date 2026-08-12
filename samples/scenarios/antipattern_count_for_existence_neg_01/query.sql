-- A genuine count that is used as a number, not as an existence test.
SELECT COUNT(*) AS OrderCount FROM dbo.Orders WHERE CustomerId = 42;
