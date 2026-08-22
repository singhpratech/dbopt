-- SUBSTRING is applied to the VARIABLE @header; the column `number` is only
-- the position argument and is never transformed.
SELECT number
FROM master.dbo.spt_values
WHERE type = 'P' AND SUBSTRING(@header, number, 1) = CHAR(13);
