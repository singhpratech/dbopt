-- A conversion in a SELECT list declares no column, so column-design advice
-- about index keys has nothing to point at.
SELECT CAST(Body AS nvarchar(max)) AS BodyText,
       CONVERT(nvarchar(max), Notes) AS NotesText
FROM dbo.Messages
WHERE MessageId = 1;
