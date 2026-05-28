-- PRIMARY KEY implies a clustered index, so this is not a heap.
CREATE TABLE dbo.Good (Id int NOT NULL PRIMARY KEY, a int);
