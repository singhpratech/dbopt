-- False-positive guard: a clean, schema-qualified read with no table hints at
-- all. There is no WITH (NOLOCK) / READUNCOMMITTED, so hygiene.nolock must stay
-- silent.
SELECT  c.CustomerId,
        c.Name,
        c.Region
FROM    dbo.Customers AS c
WHERE   c.Region = 'APAC'
ORDER BY c.Name
OFFSET 0 ROWS FETCH NEXT 50 ROWS ONLY;
