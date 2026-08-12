-- Checking whether the feature is enabled is the opposite of enabling it.
IF (SELECT value_in_use FROM sys.configurations WHERE [name] = 'xp_cmdshell') = 1
    PRINT 'xp_cmdshell is enabled';
