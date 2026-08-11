-- A LEFT JOIN's ON clause filters the RIGHT side only. Every row of dbo.T is
-- still rewritten, so this is unbounded DML wearing a join's clothes.
UPDATE t SET t.Flag = 1
FROM dbo.T t
LEFT JOIN dbo.U u ON u.tid = t.Id;
