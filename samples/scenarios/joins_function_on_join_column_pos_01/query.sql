SELECT o.OrderId
FROM dbo.Orders AS o
INNER JOIN dbo.Customers AS c ON UPPER(c.Code) = o.CustomerCode;
