-- ORs on local variables inside IF conditions are procedural range checks.
-- They touch no table, and the ORs of neighbouring IFs must not be summed.
IF @Stats <= 0 OR @Stats > 100
    RAISERROR('Stats out of range', 16, 1);
IF @CompressionLevel IS NULL AND @BackupSoftware IS NULL AND (@Version >= 17 OR (@Version >= 14 AND @Edition LIKE 'Enterprise%'))
    SET @CompressionLevel = 1;
IF @MirrorCleanupMode NOT IN ('BEFORE_BACKUP', 'AFTER_BACKUP') OR @MirrorCleanupMode IS NULL
    SET @MirrorCleanupMode = 'AFTER_BACKUP';
IF @CurrentAction IS NOT NULL AND (@CurrentPageCount IS NOT NULL OR @CurrentFragmentationLevel IS NOT NULL)
    SET @Go = 1;
IF ((@OnlyModifiedStatistics = 'N' AND @Mode = 1) OR (@OnlyModifiedStatistics = 'Y' AND @ModCount > 0) OR @Force = 1)
    SET @Go = 2;
