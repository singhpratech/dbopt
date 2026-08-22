-- A cursor over a table variable of the proc's own parameters, in a script
-- that never uses `;`. The ORDER BY sorts ~40 session-private rows; the later
-- SELECT from a real table must not be folded into this statement.
DECLARE ParameterCursor CURSOR LOCAL FAST_FORWARD FOR
SELECT [Name], ValueNvarchar, CASE WHEN [ID] = MAX([ID]) OVER() THEN '' ELSE ',' END
FROM @Parameters
ORDER BY [ID] ASC

OPEN ParameterCursor

SELECT @Count = COUNT(*)
FROM dbo.CommandLog
WHERE CommandType = @CommandType
