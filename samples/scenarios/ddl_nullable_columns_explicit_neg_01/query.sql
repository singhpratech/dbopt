-- False-positive guard: every column declares its nullability explicitly
-- (NULL or NOT NULL), so there is no dependence on session ANSI_NULL_DFLT_ON.
-- ddl.nullable_columns_should_be_explicit must stay silent.
CREATE TABLE dbo.Accounts (
    AccountId   int            NOT NULL,
    DisplayName nvarchar(200)  NOT NULL,
    Email       varchar(320)   NULL,
    PhoneNumber varchar(32)    NULL
);
