-- Pre-2017 string aggregation pattern. STUFF + FOR XML PATH('') is the
-- classic CSV-builder, but it's expensive (XML serialization round-trip)
-- and unreadable. STRING_AGG (2017+) is the native replacement.
SELECT  c.CustomerId,
        STUFF((
            SELECT ',' + t.Tag
            FROM   dbo.CustomerTags AS t
            WHERE  t.CustomerId = c.CustomerId
            ORDER BY t.Tag
            FOR XML PATH(''), TYPE
        ).value('.', 'nvarchar(max)'), 1, 1, '') AS TagList
FROM    dbo.Customers AS c;
