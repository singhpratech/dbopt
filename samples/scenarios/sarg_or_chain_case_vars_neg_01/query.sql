-- Every OR here tests a parameter, not a column. A `@p = 0 OR @p IS NULL`
-- guard cannot lose an index seek because no index is consulted.
SELECT g.session_id
FROM dbo.Grants AS g
WHERE g.grant_time >= @since
  AND 1 = CASE WHEN @IncludeMemoryGrants = 0 OR @IncludeMemoryGrants IS NULL THEN 0
               WHEN @Filter IS NULL OR CONVERT(smallint, @Filter) = 0 OR @Mode = 2 THEN 1
               ELSE 0 END;
