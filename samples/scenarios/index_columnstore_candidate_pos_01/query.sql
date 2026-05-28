-- Single-table scan + aggregation by a low-cardinality column. Textbook
-- columnstore candidate: a rowstore plan has to read every page, while
-- a clustered columnstore index would push the aggregation into batch
-- mode and slash CPU + IO by an order of magnitude.
SELECT  s.Region,
        SUM(s.Amount)     AS TotalAmount,
        COUNT(*)          AS RowCount
FROM    dbo.Sales AS s
GROUP BY s.Region;
