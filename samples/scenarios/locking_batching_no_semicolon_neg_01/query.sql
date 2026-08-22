-- Scripts that never terminate statements with `;`. The UPDATE names a single
-- row by key; the range predicate two statements later belongs to a SELECT,
-- not to the UPDATE.
UPDATE dbo.Jobs
SET    Status = 'DONE'
WHERE  JobId = @JobId

SELECT j.JobId
FROM   dbo.Jobs AS j
WHERE  j.StartedAt < DATEADD(DAY, -30, GETDATE())
