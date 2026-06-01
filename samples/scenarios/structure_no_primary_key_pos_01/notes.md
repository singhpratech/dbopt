A 240k-row table that has a CLUSTERED index on ShippedAt but no PRIMARY KEY
(`is_primary_key: false` on its only index, no PK in the bundle). Because the
data is clustered (partition `index_name` is not null) it is NOT a heap, so only
`structure.no_primary_key` should fire. Tables without a primary key break
reliable updates, replication, and most tooling.
