-- UNION ALL already skips the distinct sort.
SELECT OrderId FROM dbo.OpenOrders
UNION ALL
SELECT OrderId FROM dbo.ClosedOrders;
