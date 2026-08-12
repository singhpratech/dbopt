-- Both sides are bare columns, so the join predicate stays seekable.
SELECT o.OrderId
FROM dbo.Orders AS o
INNER JOIN dbo.Customers AS c ON c.Code = o.CustomerCode;
