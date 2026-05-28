-- False-positive guard: the SAFE isolation level. SET TRANSACTION ISOLATION
-- LEVEL READ COMMITTED is the default, non-dirty-read level. The rule only fires
-- on READ UNCOMMITTED, so it must stay silent here.
SET TRANSACTION ISOLATION LEVEL READ COMMITTED;

SELECT a.AccountId, a.Balance
FROM   dbo.Accounts AS a
WHERE  a.AccountId = 4242;
