-- Clustered index on a GUID-like leading column with no FILLFACTOR. Random
-- NEWID() inserts land at arbitrary B-tree leaves; without page slack every
-- insert risks a page split, fragmenting the index over time.
CREATE CLUSTERED INDEX CIX_Sessions_SessionGuid
    ON dbo.Sessions (SessionGuid);
