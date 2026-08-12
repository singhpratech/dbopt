-- XACT_ABORT is set, just not written first in the option list.
SET NOCOUNT, XACT_ABORT ON;
BEGIN TRANSACTION;
UPDATE dbo.Accounts SET Balance = Balance - 100 WHERE AccountId = 1;
UPDATE dbo.Ledger   SET Amount  = 100           WHERE LedgerId  = 9;
COMMIT TRANSACTION;
