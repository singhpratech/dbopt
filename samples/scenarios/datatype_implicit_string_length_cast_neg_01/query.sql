-- An explicit length: the conversion is predictable.
SELECT CAST(Notes AS varchar(400)) AS ShortNotes FROM dbo.Orders WHERE OrderId = 1;
