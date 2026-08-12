-- WHEN/THEN are CASE keywords as well as MERGE keywords. Closing the predicate
-- on them made every UDF inside or after a CASE invisible to this rule.
SELECT Id FROM dbo.Orders
WHERE 1 = CASE WHEN Total > 5 THEN 1 ELSE 0 END
  AND dbo.fn_Vat(Total) > 2;
