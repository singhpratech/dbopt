-- A table variable cannot carry a filtered index; the predicate is on a
-- session-private @FileList that a restore proc just populated.
DECLARE @FileList TABLE (BackupFile nvarchar(255), BackupPath nvarchar(255));
DECLARE @CurrentBackupPathFull nvarchar(255) = N'\\backup\full\';
UPDATE @FileList SET BackupPath = @CurrentBackupPathFull
WHERE BackupPath IS NULL;
