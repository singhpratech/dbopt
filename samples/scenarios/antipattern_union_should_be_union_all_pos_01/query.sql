SELECT OrderId FROM dbo.OpenOrders
UNION
SELECT OrderId FROM dbo.ClosedOrders;
