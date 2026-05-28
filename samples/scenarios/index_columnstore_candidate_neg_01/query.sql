-- False-positive guard: a narrow point-lookup that returns a single row by key.
-- There is no aggregate and no GROUP BY, so this is an OLTP seek, not the wide
-- analytical scan that warrants a columnstore index.
-- index.missing_columnstore_opportunity must stay silent.
SELECT s.SaleId, s.Region, s.Amount
FROM dbo.Sales AS s
WHERE s.SaleId = 100873;
