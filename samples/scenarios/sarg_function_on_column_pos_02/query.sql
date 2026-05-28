SELECT OrderId, OrderDate, TotalCents
FROM dbo.Orders
WHERE CAST(OrderDate AS date) = '2026-01-15';
