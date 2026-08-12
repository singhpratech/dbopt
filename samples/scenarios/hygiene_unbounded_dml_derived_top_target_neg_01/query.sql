-- Here the TOP-bounded derived table IS the target's source.
UPDATE QueueDatabase SET x = 1
FROM (SELECT TOP 1 y FROM dbo.Q) QueueDatabase;
