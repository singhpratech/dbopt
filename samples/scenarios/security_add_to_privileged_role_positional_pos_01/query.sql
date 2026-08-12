-- sp_addsrvrolemember takes the LOGIN first and the ROLE second — the opposite
-- of sp_addrolemember. Reading "the first string" as the role meant the most
-- ordinary way to write a sysadmin grant produced nothing at all.
EXEC sp_addsrvrolemember 'AppLogin', 'sysadmin';
