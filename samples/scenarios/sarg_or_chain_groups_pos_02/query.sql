-- Counting per group must not silence a real chain: four ORs on one column in
-- a single parenthesised group, AND-ed with an unrelated filter, still fire.
SELECT o.OrderId
FROM dbo.Orders AS o
WHERE o.CustomerId = @cust
  AND (o.Status = 'New' OR o.Status = 'Paid' OR o.Status = 'Packed' OR o.Status = 'Shipped' OR o.Status = 'Closed');
