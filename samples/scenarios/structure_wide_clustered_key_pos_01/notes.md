An 880k-row table whose PRIMARY KEY (and therefore clustered key) spans five
columns (TenantId, WarehouseId, OrderId, LineNo, SkuId). Every nonclustered
index stores the full clustered key as its row locator, so a 5-column key
inflates all of them. The rule fires at >= 5 key columns. The table has a PK and
is clustered, so `structure.no_primary_key` and `structure.heap_table` stay quiet.
