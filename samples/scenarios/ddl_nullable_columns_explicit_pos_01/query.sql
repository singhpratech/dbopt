-- Columns without an explicit NULL / NOT NULL declaration. Their nullability
-- silently follows the session ANSI_NULL_DFLT_ON setting, so the same script
-- can produce different schemas depending on who runs it.
CREATE TABLE dbo.Contacts (
    ContactId   int            NOT NULL,
    FullName    nvarchar(200),
    Email       varchar(320),
    PhoneNumber varchar(32)    NOT NULL
);
