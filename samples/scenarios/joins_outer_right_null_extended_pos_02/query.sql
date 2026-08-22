-- Customers is the null-extended side of this RIGHT JOIN, so the WHERE demotes it.
SELECT c.CustomerId, o.OrderId
FROM dbo.Customers AS c
RIGHT JOIN dbo.Orders AS o ON o.CustomerId = c.CustomerId
WHERE c.Region = 'EU';
