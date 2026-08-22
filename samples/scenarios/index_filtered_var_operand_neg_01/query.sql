-- `WHERE @sa = 0` tests a local variable, not a column: there is no column to
-- put a filtered index on. (Paraphrased from a health-check proc that uses a
-- variable predicate as a branch switch over an inline VALUES list.)
DECLARE @sa bit = 0;
INSERT INTO #BlitzResults (CheckID, Priority, Finding, Details)
SELECT v.CheckID, v.Priority, v.Finding, v.Details
FROM (VALUES (1, 10, N'Security', N'Not sysadmin')) AS v (CheckID, Priority, Finding, Details)
WHERE @sa = 0;
