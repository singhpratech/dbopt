UPDATE dbo.Accounts SET Balance = 0 WHERE AccountId = 1;
COMMIT TRANSACTION;
