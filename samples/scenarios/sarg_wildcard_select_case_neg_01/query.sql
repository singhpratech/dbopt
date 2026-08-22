-- A LIKE inside a CASE in the SELECT list classifies a value; it is not a
-- search condition and no seek is at stake.
SELECT s.variable_name,
       CASE WHEN s.variable_datatype NOT LIKE '%binary%' AND s.compile_time_value IS NOT NULL THEN 1 ELSE 0 END AS is_literal
FROM #variable_info AS s
WHERE s.query_hash = @hash;
