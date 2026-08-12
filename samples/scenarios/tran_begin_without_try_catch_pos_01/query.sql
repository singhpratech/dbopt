BEGIN TRANSACTION;
UPDATE dbo.Accounts SET Balance = Balance - 100 WHERE AccountId = 1;
UPDATE dbo.Accounts SET Balance = Balance + 100 WHERE AccountId = 2;
COMMIT TRANSACTION;
