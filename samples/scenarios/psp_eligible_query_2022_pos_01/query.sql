-- Common PSP-eligible shape on 2022+: status filter with skewed distribution.
-- 99% of rows are 'Closed', a fraction of a percent are 'Pending'. Parameter-
-- Sensitive Plan optimization can produce one plan per cardinality bucket.
-- The `OPTION (RECOMPILE)` here disables PSP entirely — a future rule should
-- flag the trade-off.
CREATE OR ALTER PROCEDURE dbo.GetOrdersByStatus
    @status varchar(20)
AS
BEGIN
    SET NOCOUNT ON;

    SELECT  o.OrderId,
            o.CustomerId,
            o.TotalCents,
            o.OrderDate
    FROM    dbo.Orders AS o
    WHERE   o.Status = @status
    OPTION (RECOMPILE);
END;
