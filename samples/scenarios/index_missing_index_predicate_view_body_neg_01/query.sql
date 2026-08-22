-- Index advice on a VIEW or inline-TVF body is not actionable where it is
-- reported (the index belongs on the base table, whose DDL is elsewhere), so
-- index.missing_index_from_predicate must stay silent inside these bodies.
CREATE VIEW dbo.vShippedOrders WITH SCHEMABINDING AS
SELECT o.OrderId, o.CustomerId, o.OrderDate
FROM   dbo.Orders AS o
WHERE  o.Status = 'Shipped' AND o.Region = 'EU';
GO
CREATE FUNCTION dbo.fnOpenOrders(@customerId int)
RETURNS TABLE
AS RETURN
(
    SELECT o.OrderId, o.OrderDate
    FROM   dbo.Orders AS o
    WHERE  o.CustomerId = @customerId AND o.Status = 'Open'
);
GO
