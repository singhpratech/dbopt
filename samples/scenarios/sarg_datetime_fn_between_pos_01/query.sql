-- YEAR(col) BETWEEN … wraps the date column in a function, so the index on
-- PlacedAt cannot be seeked and the whole table is scanned.
SELECT OrderId FROM dbo.Orders WHERE YEAR(PlacedAt) BETWEEN 2024 AND 2025;
