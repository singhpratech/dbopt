-- The same predicate inside a PROCEDURE body is still actionable: the proc
-- runs the query, so the index it needs is a real recommendation.
CREATE PROCEDURE dbo.uspShippedOrdersEU
AS
BEGIN
    SET NOCOUNT ON;
    SELECT o.OrderId, o.CustomerId, o.OrderDate
    FROM   dbo.Orders AS o
    WHERE  o.Status = 'Shipped' AND o.Region = 'EU';
END
GO
