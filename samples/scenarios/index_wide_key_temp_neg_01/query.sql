-- A session-scoped #temp work table keyed by a wait-type name. It carries no
-- nonclustered indexes, so there is nothing for a wide key to inflate.
CREATE TABLE #WaitCategories (
    WaitType     nvarchar(60) NOT NULL PRIMARY KEY CLUSTERED,
    WaitCategory nvarchar(128) NOT NULL,
    Ignorable    bit NOT NULL DEFAULT 0
);
