-- EXISTS expresses "customers that have at least one order" without fanning
-- out the rowset and then collapsing it again.
SELECT c.CustomerId, c.Name
FROM dbo.Customers AS c
WHERE EXISTS (SELECT 1 FROM dbo.Orders o WHERE o.CustomerId = c.CustomerId);
