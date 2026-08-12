-- A stored procedure whose *name* merely mentions the shell is not the shell.
EXEC dbo.Log_xp_cmdshell_AuditAttempt @Reason = 'blocked';
