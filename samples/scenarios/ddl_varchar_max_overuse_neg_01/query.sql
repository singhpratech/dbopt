-- Bounded NVARCHAR(400) stays in-row and remains indexable.
CREATE TABLE dbo.Doc (Id int, Body nvarchar(400));
