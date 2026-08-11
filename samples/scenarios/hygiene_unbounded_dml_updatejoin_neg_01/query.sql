-- UPDATE ... FROM ... JOIN is the most common bulk-update shape in production
-- T-SQL. It is bounded by the join predicate, and `a` is an alias that cannot
-- be schema-qualified.
UPDATE a SET a.NextBillDate = d.NextDate
FROM dbo.Accounts a
JOIN dbo.Due d ON d.AccountId = a.AccountId;
