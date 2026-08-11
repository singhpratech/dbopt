-- A textbook MERGE upsert. The UPDATE arm is scoped by the MERGE's own ON
-- clause, so it is not unbounded DML, and `SET` is a keyword, not a table.
MERGE dbo.Target AS t
USING dbo.Source AS s ON t.Id = s.Id
WHEN MATCHED THEN UPDATE SET t.Val = s.Val
WHEN NOT MATCHED THEN INSERT (Id, Val) VALUES (s.Id, s.Val);
