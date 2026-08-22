-- The variable only ever holds one constant literal: nothing to parameterize.
DECLARE @sql nvarchar(max) = N'DBCC CHECKDB WITH NO_INFOMSGS;';
EXEC (@sql);
