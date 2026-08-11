-- A range on a date plus an equality on a flag is a textbook bulk delete. An
-- equality sitting beside a range must not be read as "this names one row".
DELETE FROM dbo.Events
WHERE CreatedUtc < DATEADD(day, -90, SYSUTCDATETIME())
  AND IsArchived = 1;
