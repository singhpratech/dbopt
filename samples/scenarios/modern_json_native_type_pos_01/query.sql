-- Document column stored as nvarchar(max) with an ISJSON check constraint. On
-- 2025+ the native json type stores parsed binary: faster reads, in-place
-- .modify(), and better compression than the validated-string pattern.
CREATE TABLE dbo.Events (
    EventId  bigint        NOT NULL,
    Document nvarchar(max) NOT NULL CONSTRAINT CK_Events_Document CHECK (ISJSON(Document) = 1),
    LoadedAt datetime2(3)  NOT NULL
);
