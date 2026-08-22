-- The STUFF((SELECT ... FOR XML PATH('')) string-aggregation idiom, also inside
-- CROSS APPLY: there is no OUTER APPLY rewrite for a FOR XML subquery.
SELECT si.index_id,
       key_columns = STUFF((SELECT N', ' + QUOTENAME(id2.column_name)
                            FROM #index_details AS id2
                            WHERE id2.index_id = si.index_id
                            ORDER BY id2.key_ordinal
                            FOR XML PATH(''), TYPE).value('.', 'nvarchar(max)'), 1, 2, N'')
FROM #index_sanity AS si;

UPDATE #IndexSanity
SET include_column_names = D3.include_column_names
FROM #IndexSanity AS si
CROSS APPLY (SELECT RTRIM(STUFF((SELECT N', ' + QUOTENAME(c.column_name)
                                 FROM #IndexColumns AS c
                                 WHERE c.index_id = si.index_id AND c.is_included_column = 1
                                 FOR XML PATH('')), 1, 1, N'')) AS include_column_names) AS D3;
