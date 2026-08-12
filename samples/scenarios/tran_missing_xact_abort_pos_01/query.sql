BEGIN TRANSACTION;
UPDATE dbo.Accounts SET Balance = Balance - 100 WHERE AccountId = 1;
UPDATE dbo.Ledger  SET Amount  = 100           WHERE LedgerId  = 9;
INSERT INTO dbo.Audit (Note) VALUES ('transfer');
COMMIT TRANSACTION;
