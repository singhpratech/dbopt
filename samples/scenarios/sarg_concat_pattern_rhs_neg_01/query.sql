-- The concatenation builds the LIKE pattern on the RIGHT; the compared column
-- `the_path` stays bare. The leading wildcard is the real reason it scans,
-- and that is a different rule's report.
SELECT b.the_path
FROM #blocking AS b
JOIN #sessions AS s ON s.session_id = b.blocked_id
WHERE b.the_path NOT LIKE '%.' + CONVERT(varchar(8000), s.session_id) + '.%';
