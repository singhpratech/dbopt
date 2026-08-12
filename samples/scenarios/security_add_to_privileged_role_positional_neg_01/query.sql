-- Same proc, same shape, but the role is not a privileged one.
EXEC sp_addsrvrolemember 'AppLogin', 'dbcreator';
