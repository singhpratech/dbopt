-- The statement that ENABLES the feature names it in a string literal, which is
-- the one spelling a Word-token-only scan could never see.
EXEC sp_configure 'xp_cmdshell', 1;
RECONFIGURE;
