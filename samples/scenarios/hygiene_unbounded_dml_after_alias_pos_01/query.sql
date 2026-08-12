-- `AFTER` here is a table alias, not a trigger event.
SELECT x FROM dbo.Appointments AFTER
UPDATE dbo.T SET x = 1;
