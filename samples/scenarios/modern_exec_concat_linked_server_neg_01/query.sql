-- Pass-through execution on a linked server has no sp_executesql form.
EXEC ('SELECT name FROM sys.databases WHERE name = ''' + @db + '''') AT [REMOTE01];
