SELECT CustomerId, LastName
FROM dbo.Customers
WHERE UPPER(LastName) = 'SMITH';
