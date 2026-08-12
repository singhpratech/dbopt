-- `month`, `date` and `text` are extremely common COLUMN names. Excluding
-- type/datepart words in every argument position made the rule silently blind
-- to them; the exclusion belongs only where a type or datepart actually goes.
SELECT Id FROM dbo.T WHERE UPPER(month) = 'JAN';
SELECT Id FROM dbo.U WHERE LEFT([Text], 45) = N'Database Instant File Initialization: enabled';
