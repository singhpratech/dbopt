SELECT mi.index_handle, mi.magic_benefit_number
FROM #MissingIndexes AS mi
LEFT JOIN dbo.CreateDates AS cd ON cd.database_id = mi.database_id
WHERE @ShowAllMissingIndexRequests = 1
   OR (mi.magic_benefit_number / CASE WHEN cd.create_days < @DaysUptime THEN cd.create_days ELSE @DaysUptime END) >= 100000
ORDER BY mi.magic_benefit_number DESC;
