-- A wide DISTINCT over a join is the classic band-aid: the join fans rows out
-- and DISTINCT sorts them back down again.
SELECT DISTINCT o.OrderId, o.CustomerId, o.PlacedAt, o.Total, o.Status, o.Channel
FROM dbo.Orders AS o
JOIN dbo.OrderLines AS l ON l.OrderId = o.OrderId;
