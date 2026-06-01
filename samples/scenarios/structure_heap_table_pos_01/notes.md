A 5.4M-row table whose base data lives in a HEAP (clustered partition has
`index_name: null`). It carries a nonclustered primary key (PK_EventLog), so
`structure.no_primary_key` must stay quiet — the only structural defect is the
missing clustered index. Heaps fragment, can't be range-seeked, and accumulate
forward pointers on UPDATE; at this row count the rule should escalate to Error.
