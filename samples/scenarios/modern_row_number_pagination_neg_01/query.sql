-- False-positive guard: pagination already uses the modern OFFSET/FETCH clause
-- rather than a ROW_NUMBER() window filtered by a row-range predicate. There is
-- no ROW_NUMBER() call, so modern.row_number_pagination_replaces_offset_fetch
-- must stay silent.
SELECT OrderId, CustomerId, PlacedAt
FROM dbo.Orders
ORDER BY PlacedAt DESC
OFFSET 40 ROWS FETCH NEXT 20 ROWS ONLY;
