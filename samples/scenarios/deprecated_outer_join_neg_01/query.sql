-- False-positive guard: modern ANSI LEFT OUTER JOIN syntax. The rule only fires
-- on the removed *= / =* legacy outer-join operators, so a clean ANSI join must
-- stay silent.
SELECT  c.CustomerId,
        c.Name,
        o.OrderId
FROM    dbo.Customers AS c
LEFT OUTER JOIN dbo.Orders AS o
    ON o.CustomerId = c.CustomerId
WHERE   c.Region = 'EMEA';
