-- The transaction is wrapped, so a mid-batch failure is caught and rolled back.
BEGIN TRY
    BEGIN TRANSACTION;
    UPDATE dbo.Accounts SET Balance = Balance - 100 WHERE AccountId = 1;
    COMMIT TRANSACTION;
END TRY
BEGIN CATCH
    IF XACT_STATE() <> 0 ROLLBACK TRANSACTION;
    THROW;
END CATCH
