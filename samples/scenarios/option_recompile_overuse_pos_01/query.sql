-- Reporting proc that sprays OPTION (RECOMPILE) on every statement.
-- Convenient escape hatch for parameter sniffing, but at this volume
-- every call burns CPU on compilation and drops plan-cache observability.
CREATE OR ALTER PROCEDURE dbo.BuildDailySummary
    @customerId int,
    @asOf       date
AS
BEGIN
    SET NOCOUNT ON;

    SELECT COUNT(*) AS OrderCount
    FROM   dbo.Orders
    WHERE  CustomerId = @customerId
      AND  OrderDate  = @asOf
    OPTION (RECOMPILE);

    SELECT SUM(TotalCents) AS Revenue
    FROM   dbo.Orders
    WHERE  CustomerId = @customerId
      AND  OrderDate  = @asOf
    OPTION (RECOMPILE);

    SELECT TOP (50) li.Sku, SUM(li.Quantity) AS Units
    FROM   dbo.LineItems AS li
    JOIN   dbo.Orders   AS o ON o.OrderId = li.OrderId
    WHERE  o.CustomerId = @customerId
      AND  o.OrderDate  = @asOf
    GROUP BY li.Sku
    ORDER BY Units DESC
    OPTION (RECOMPILE);
END;
