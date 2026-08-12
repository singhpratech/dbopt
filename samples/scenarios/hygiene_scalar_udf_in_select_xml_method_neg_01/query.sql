-- `.value()` / `.nodes()` / `.query()` are XML methods on a column, not
-- schema-qualified scalar UDFs. There is no UDF here to inline.
SELECT p.ContactInfo.value('(/Person/Name)[1]', 'nvarchar(50)') AS PersonName,
       p.ContactInfo.query('/Person/Address') AS Addr
FROM dbo.People AS p;
