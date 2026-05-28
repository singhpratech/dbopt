-- Classic upsert implemented with MERGE. MERGE has a long history of
-- concurrency and correctness bugs (duplicate-key races, Halloween problems);
-- a staged UPDATE + INSERT under HOLDLOCK is the safer pattern.
SET NOCOUNT ON;

MERGE dbo.Inventory AS target
USING @incoming AS source
    ON target.Sku = source.Sku
WHEN MATCHED THEN
    UPDATE SET target.Quantity = source.Quantity
WHEN NOT MATCHED BY TARGET THEN
    INSERT (Sku, Quantity) VALUES (source.Sku, source.Quantity);
