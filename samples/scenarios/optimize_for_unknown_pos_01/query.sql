-- Legacy parameter-sniffing workaround. `OPTIMIZE FOR UNKNOWN` forces the
-- optimizer to use the average-density estimate instead of the actual
-- parameter value, which is almost always worse than letting PSP (2022+)
-- or a properly indexed predicate do its job.
DECLARE @customerId int = 4711;

SELECT  o.OrderId,
        o.OrderDate,
        o.TotalCents
FROM    dbo.Orders AS o
WHERE   o.CustomerId = @customerId
OPTION (OPTIMIZE FOR (@customerId UNKNOWN));
