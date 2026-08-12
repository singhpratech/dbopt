-- UPDATE STATISTICS is maintenance DDL; STATISTICS is a keyword, not a table.
UPDATE STATISTICS dbo.Orders WITH FULLSCAN;
