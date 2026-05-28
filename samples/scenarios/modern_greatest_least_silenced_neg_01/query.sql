-- Version-silence: this CASE WHEN a > b THEN a ELSE b END is exactly the shape
-- modern.greatest_least_replaces_case_when fires on (GREATEST/LEAST are 2022+).
-- Target is 2017, below the 2022 gate, so the rule must stay silent.
SELECT  t.Id,
        CASE WHEN t.PriceA > t.PriceB THEN t.PriceA ELSE t.PriceB END AS MaxPrice
FROM    dbo.Quotes AS t;
