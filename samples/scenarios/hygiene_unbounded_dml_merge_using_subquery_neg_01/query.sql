-- The back-scan must treat `USING (SELECT ...)` as part of the MERGE, not as a
-- statement boundary, and must step over the comment before UPDATE.
MERGE dbo.Target AS t
USING (SELECT Id, Amount FROM dbo.Staging WHERE Amount > 0) AS s ON s.Id = t.Id
WHEN MATCHED THEN /* upsert */ UPDATE SET t.Amount = s.Amount;
