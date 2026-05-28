-- False-positive guard: FOR XML used to produce ACTUAL XML output, not the
-- STUFF(... FOR XML PATH('')) CSV-concatenation idiom. The rule only fires on
-- FOR XML PATH('') (empty-string path), so a real PATH('row')/RAW shape with a
-- root element must stay silent.
SELECT  o.OrderId,
        o.CustomerId,
        o.OrderDate
FROM    dbo.Orders AS o
WHERE   o.Status = 1
FOR XML PATH('Order'), ROOT('Orders'), ELEMENTS;
