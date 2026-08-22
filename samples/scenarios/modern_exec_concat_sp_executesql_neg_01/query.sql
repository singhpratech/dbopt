-- EXEC sp_executesql with a prebuilt statement, directly followed by another
-- statement that builds a string. The call itself concatenates nothing.
EXEC sp_executesql @InnerStringToExecute

SET @StringToExecute = N'SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED;' + @crlf;
