-- Persisted-looking computed column whose expression is a scalar UDF call. Any
-- query that touches FullName is forced to evaluate dbo.fnFullName row-by-row,
-- blocking inlining and parallelism.
CREATE TABLE dbo.People (
    PersonId  int          NOT NULL,
    FirstName nvarchar(100) NOT NULL,
    LastName  nvarchar(100) NOT NULL,
    FullName  AS dbo.fnFullName(FirstName, LastName)
);
