The #1 real-world missing-index shape the token rules otherwise skip: a clean
two-table INNER equijoin (`dbo.Customers` ⨝ `dbo.Orders` on `CustomerId`) with a
sargable equality filter (`o.Status = 'OPEN'`) on the joined/probed table.

The probed table here is `Orders`. The suggested covering index keys on the
equality-filter column first, then the join key — `(Status, CustomerId)` — and
INCLUDEs the projected `Orders` columns (`TotalCents`; `OrderId` is the join-side
projection). Should fire `index.join_filter_missing_index`.
