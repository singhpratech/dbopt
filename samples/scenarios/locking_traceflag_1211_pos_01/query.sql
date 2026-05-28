-- TF 1211 disables lock escalation globally — usually applied as a quick
-- fix for an escalation incident and then forgotten. Without escalation
-- the lock manager memory grows unbounded under sustained DML, eventually
-- starving other sessions.
DBCC TRACEON (1211, -1);
