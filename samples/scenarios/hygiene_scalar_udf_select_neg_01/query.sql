-- Only a built-in aggregate in the projection; no schema.fn() call.
SELECT o.Id, SUM(o.Total) AS T FROM dbo.Orders o GROUP BY o.Id;
