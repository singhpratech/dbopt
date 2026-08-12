-- Added to an ordinary application role, not a privileged one.
EXEC sp_addrolemember @rolename = 'ReportReaders', @membername = 'AppUser';
