# ts.missing_index_dmv (negative) — empty missing-index DMV must stay silent

Grounds the MS-Learn caveat that the missing-index DMVs clear on restart and cap at 600
rows, so an EMPTY `missing_indexes` set is NOT proof of "no missing indexes". The honest
behavior is the absence of a finding (a blank, never an invented "you have no missing
indexes" claim).

This negative bundle has a healthy read-heavy PK index (14,200 seeks, only 220 updates) and
an empty `missing_indexes` list. The analyzer must:
- NOT emit `dmv.missing_index` (nothing was suggested),
- NOT emit `dmv.unused_index` (the index is read, not pure write tax),
- NOT emit `dmv.duplicate_or_overlapping_index` (only one index on the table).

Reference: https://learn.microsoft.com/en-us/sql/relational-databases/indexes/tune-nonclustered-missing-index-suggestions
