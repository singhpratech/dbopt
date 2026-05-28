-- False-positive guard: the recommended staged upsert pattern -- an UPDATE ...
-- FROM followed by an INSERT ... WHERE NOT EXISTS, both inside an explicit
-- transaction with HOLDLOCK. No MERGE statement is used, so
-- hygiene.merge_statement_for_upsert must stay silent.
BEGIN TRANSACTION;

UPDATE t
SET t.Quantity = s.Quantity
FROM dbo.Inventory AS t WITH (HOLDLOCK)
INNER JOIN @incoming AS s ON t.Sku = s.Sku;

INSERT INTO dbo.Inventory (Sku, Quantity)
SELECT s.Sku, s.Quantity
FROM @incoming AS s
WHERE NOT EXISTS (SELECT 1 FROM dbo.Inventory AS t WHERE t.Sku = s.Sku);

COMMIT TRANSACTION;
