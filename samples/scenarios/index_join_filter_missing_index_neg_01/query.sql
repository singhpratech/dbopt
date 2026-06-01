-- False-positive guard. Three tables (two JOINs) — outside the conservative
-- two-table shape this rule supports — so index.join_filter_missing_index must
-- stay silent. Inferring a single covering index across a multi-join chain is
-- exactly the kind of guesswork the rule deliberately refuses to make offline.
SELECT  c.Name,
        o.OrderId,
        li.Sku
FROM    dbo.Customers AS c
INNER JOIN dbo.Orders    AS o  ON o.CustomerId = c.CustomerId
INNER JOIN dbo.LineItems AS li ON li.OrderId   = o.OrderId
WHERE   o.Status = 'OPEN';
