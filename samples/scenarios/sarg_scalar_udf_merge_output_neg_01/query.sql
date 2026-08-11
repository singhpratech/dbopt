-- `dbo.Audit (Id)` is an OUTPUT target with a column list, not a scalar UDF
-- call in a predicate. The MERGE's ON must not leave the predicate open.
MERGE dbo.Target AS t
USING dbo.Source AS s ON t.Id = s.Id
WHEN MATCHED THEN UPDATE SET t.Val = s.Val
OUTPUT inserted.Id INTO dbo.Audit (Id);
