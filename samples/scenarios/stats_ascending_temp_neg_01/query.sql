-- #IndexSanity was populated a few statements earlier in the same execution,
-- so its statistics cannot lag its inserts.
SELECT  i.database_name, i.index_name, i.create_date
FROM    #IndexSanity AS i
WHERE   i.create_date >= DATEADD(dd, -7, GETDATE());
