-- 2019 target: the 2016/2017 constructs are fine there, but the 2022 clause
-- syntax (named WINDOW, IS DISTINCT FROM) and GREATEST are not.
SELECT STRING_AGG(p.Name, ',') FROM dbo.Products AS p;
SELECT o.CustomerId,
       SUM(o.TotalCents) OVER w AS RunningTotal
FROM   dbo.Orders AS o
WINDOW w AS (PARTITION BY o.CustomerId ORDER BY o.OrderDate);
SELECT o.OrderId FROM dbo.Orders AS o WHERE o.ShippedAt IS NOT DISTINCT FROM o.DeliveredAt;
SELECT GREATEST(o.TotalCents, 0) FROM dbo.Orders AS o;
