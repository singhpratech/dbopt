-- A function in a SELECT projection is not a predicate. There is no index to
-- lose, and the predicate region must not leak past the statement that opened it.
SELECT Id FROM dbo.Items WHERE Id > 0;
SELECT CASE WHEN LEFT(DatabaseItem, 1) = '[' THEN 1 ELSE 0 END AS IsQuoted
FROM dbo.Config;
