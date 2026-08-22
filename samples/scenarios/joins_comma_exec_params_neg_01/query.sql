-- Commas separate procedure arguments; the un-terminated SELECT before it
-- must not swallow the EXECUTE into its FROM list.
SELECT @Count = COUNT(*) FROM dbo.Orders
EXECUTE sp_executesql @stmt = @sql, @params = N'@Count int', @Count = @Count
EXECUTE @rc = dbo.CommandExecute @DatabaseContext = @db, @Command = @cmd
