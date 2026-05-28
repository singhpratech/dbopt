CREATE PROCEDURE dbo.GetOrders AS
BEGIN
    SELECT OrderId FROM dbo.Orders WHERE Status = 1;
END