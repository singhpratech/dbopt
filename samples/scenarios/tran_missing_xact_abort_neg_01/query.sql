-- XACT_ABORT ON means any statement error aborts the whole transaction.
SET XACT_ABORT ON;
BEGIN TRANSACTION;
UPDATE dbo.Accounts SET Balance = Balance - 100 WHERE AccountId = 1;
UPDATE dbo.Ledger  SET Amount  = 100           WHERE LedgerId  = 9;
COMMIT TRANSACTION;
