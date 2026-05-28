-- False-positive guard: a computed column defined by an inline arithmetic
-- expression -- no schema-qualified scalar UDF call. Inline computed columns do
-- not block 2019+ scalar UDF inlining, so plan.scalar_udf_in_computed_column
-- must stay silent.
CREATE TABLE dbo.OrderLines (
    OrderLineId int      NOT NULL,
    Quantity    int      NOT NULL,
    UnitCents   bigint   NOT NULL,
    TotalCents  AS (Quantity * UnitCents) PERSISTED
);
