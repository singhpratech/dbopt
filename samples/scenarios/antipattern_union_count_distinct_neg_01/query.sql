-- UNION of FROM-less scalar selects counted by COUNT(*): the dedupe is the
-- point (detect colliding file extensions). UNION ALL would always return 3.
IF @CleanupTime IS NOT NULL
   AND (SELECT COUNT(*) FROM (SELECT @FileExtensionFull AS FileExtension UNION SELECT @FileExtensionDiff UNION SELECT @FileExtensionLog) AS F) <> 3
BEGIN
    INSERT INTO @Errors ([Message], Severity, [State])
    VALUES ('The file extensions are not unique.', 16, 1);
END
