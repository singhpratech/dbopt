-- Page slicing on a CTE-projected ROW_NUMBER(): the consuming SELECT filters
-- the window alias with a lower and an upper bound.
WITH paged AS (
    SELECT OrderId, PlacedAt,
           ROW_NUMBER() OVER (ORDER BY PlacedAt DESC) AS rn
    FROM dbo.Orders
)
SELECT OrderId, PlacedAt
FROM paged
WHERE rn > @first AND rn <= @last
ORDER BY rn;
