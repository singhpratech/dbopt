-- NOT EXISTS inside a CASE in the select list is a semi-join predicate, not a
-- scalar lookup.
SELECT ia.index_name,
       CASE WHEN ia.action = N'DISABLE'
             AND NOT EXISTS (SELECT 1 FROM #index_details AS id_uc
                             WHERE id_uc.index_hash = ia.index_hash
                               AND id_uc.is_unique_constraint = 1)
            THEN 'YES' ELSE 'NO' END AS can_disable
FROM #index_actions AS ia;
