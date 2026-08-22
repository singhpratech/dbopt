-- `@MaxTransferSize % 64` is arithmetic on a local VARIABLE inside an IF:
-- there is no column, no table and no index to seek.
IF @MaxTransferSize % 64 <> 0
BEGIN
    RAISERROR('MAXTRANSFERSIZE must be a multiple of 64 KB', 16, 1);
    RETURN;
END;
SET @Msg = CASE WHEN @CleanupTime % 24 > 0 THEN 'partial day' ELSE 'whole days' END;
