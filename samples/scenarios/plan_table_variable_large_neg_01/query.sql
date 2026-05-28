-- False-positive guard: a small table variable populated with a handful of
-- literal rows via INSERT ... VALUES, not from a SELECT over a real table. With
-- only a few known rows the 1-row cardinality estimate is fine, so
-- plan.table_variable_large must stay silent.
DECLARE @StatusCodes TABLE (Code char(1) NOT NULL, Label nvarchar(20) NOT NULL);

INSERT INTO @StatusCodes (Code, Label)
VALUES ('O', N'Open'), ('C', N'Closed'), ('P', N'Pending');

SELECT Code, Label FROM @StatusCodes;
