-- Rolling back to a savepoint unwinds part of the work; the outer transaction
-- is still open when the batch ends.
BEGIN TRANSACTION;
SAVE TRANSACTION sp1;
UPDATE dbo.Accounts SET Balance = 0 WHERE AccountId = 1;
ROLLBACK TRANSACTION sp1;
