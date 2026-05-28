-- Page 3 of search results paginated with a ROW_NUMBER() window inside a
-- derived table, then filtered by row range. OFFSET/FETCH (or keyset
-- pagination) expresses this more clearly and often plans better.
SELECT ProductId, Name, Price
FROM (
    SELECT ProductId, Name, Price,
           ROW_NUMBER() OVER (ORDER BY Name) AS rn
    FROM dbo.Products
) paged
WHERE rn BETWEEN 41 AND 60
ORDER BY rn;
