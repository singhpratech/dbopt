-- Version-silence: nvarchar(max) column with a CHECK (ISJSON(...)) constraint is
-- exactly what modern.json_native_type_opportunity fires on (native json type is
-- 2025+). Target is 2022, below the 2025 gate, so the rule must stay silent.
CREATE TABLE dbo.Documents
(
    DocId    INT IDENTITY(1,1) NOT NULL PRIMARY KEY CLUSTERED,
    Payload  NVARCHAR(MAX)     NOT NULL CHECK (ISJSON(Payload) = 1)
);
