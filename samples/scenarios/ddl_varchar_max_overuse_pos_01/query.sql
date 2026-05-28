-- NVARCHAR(MAX) when the data likely fits in-row: off-row storage, not indexable.
CREATE TABLE dbo.Doc (Id int, Body nvarchar(max));
