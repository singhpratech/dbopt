-- `Data nvarchar(max) AS JSON` inside an OPENJSON WITH clause: the AS JSON
-- modifier REQUIRES nvarchar(max); a bounded length is a syntax error.
SELECT j.Name, j.Price, j.Data
FROM OPENJSON(@json)
WITH (
    Name  nvarchar(50),
    Price money,
    Type  nvarchar(20) '$.Data.Type',
    Data  nvarchar(max) AS JSON
) AS j;
