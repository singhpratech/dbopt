-- A savepoint rollback is inner scope, not a close of an unopened transaction.
BEGIN TRANSACTION;
SAVE TRANSACTION sp1;
UPDATE dbo.Accounts SET Balance = 0 WHERE AccountId = 1;
ROLLBACK TRANSACTION sp1;
COMMIT TRANSACTION;
