-- A CTE does not survive GO, so it cannot vouch for a later UPDATE.
WITH q AS (SELECT f FROM dbo.T WHERE f = 1)
SELECT f FROM q
GO
UPDATE q SET f = 1
GO
