-- Generated IN-list from an ORM that materialized a parent-set of order IDs.
-- Long literal IN-lists balloon plan size, defeat plan-cache reuse (each new
-- length is a fresh plan), and frequently get rewritten internally as a
-- constant scan + hash join that spills.
SELECT  o.OrderId,
        o.CustomerId,
        o.TotalCents,
        o.OrderDate
FROM    dbo.Orders AS o
WHERE   o.OrderId IN (
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
    11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
    21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31, 32, 33, 34, 35, 36, 37, 38, 39, 40,
    41, 42, 43, 44, 45, 46, 47, 48, 49, 50,
    51, 52, 53, 54, 55, 56, 57, 58, 59, 60,
    61, 62, 63, 64, 65
);
