-- False-positive guard: a narrow single-column INT IDENTITY clustered primary
-- key. index.wide_clustered_key only fires on wide string PKs (>32 chars) or
-- composite PKs of 3+ columns, so this clean narrow surrogate key must stay silent.
CREATE TABLE dbo.Orders
(
    OrderId     INT IDENTITY(1,1) NOT NULL PRIMARY KEY CLUSTERED,
    CustomerId  INT               NOT NULL,
    OrderDate   DATETIME2(3)      NOT NULL,
    TotalCents  BIGINT            NOT NULL
);
