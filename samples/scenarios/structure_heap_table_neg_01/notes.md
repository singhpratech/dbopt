Same 5.4M-row table, but the base data now lives in a CLUSTERED index
(`index_name: "PK_EventLog"`, no null-index partition). It has a primary key and
the key is a single narrow column. No structural defect — `structure.heap_table`,
`structure.no_primary_key`, and `structure.wide_clustered_key` must all stay quiet.
