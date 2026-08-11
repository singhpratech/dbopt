-- OPENJSON is a built-in rowset function. It has no schema to qualify.
SELECT j.Id FROM OPENJSON(@payload) WITH (Id int '$.id') AS j;
