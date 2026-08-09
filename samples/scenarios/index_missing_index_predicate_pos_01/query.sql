-- No index is declared anywhere in this batch, so the equality filter has
-- nothing to seek: expect a concrete covering-index recommendation.
SELECT o.Id, o.Total, o.CustId, o.CreatedAt, o.Region
FROM dbo.Orders o
WHERE o.Status = 'OPEN';
