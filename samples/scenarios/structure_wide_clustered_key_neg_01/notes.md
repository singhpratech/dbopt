Same 880k-row table, but the clustered PRIMARY KEY is a two-column natural key
(OrderId, LineNo) — below the 5-column threshold. The key is narrow enough, the
table is clustered, and it has a PK, so no structure rule should fire.
