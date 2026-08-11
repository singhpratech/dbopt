-- The MERGE ends at GO, not at a semicolon. The UPDATE below is a separate
-- statement and must not inherit the MERGE's ON clause as a bound.
MERGE dbo.Target AS t
USING dbo.Source AS s ON t.Id = s.Id
WHEN MATCHED THEN UPDATE SET t.Val = s.Val
GO
UPDATE dbo.Target SET Active = 0
GO
