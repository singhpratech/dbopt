-- A user table compared to an N'…' literal in a JOIN ON is still reported;
-- only catalog sources are exempt.
SELECT o.OrderId
FROM dbo.Orders AS o
JOIN dbo.Status AS st ON st.Code = N'OPEN' AND st.Id = o.StatusId;
