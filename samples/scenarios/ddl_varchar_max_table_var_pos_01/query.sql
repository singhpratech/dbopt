-- A table variable column declared NVARCHAR(MAX) for a value that is a short
-- code: still column design, still off-row and un-indexable.
DECLARE @Codes TABLE (
    CodeId int NOT NULL,
    Code   nvarchar(max) NOT NULL
);
