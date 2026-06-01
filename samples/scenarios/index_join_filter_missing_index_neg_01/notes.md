Three-table join (Customers ⨝ Orders ⨝ LineItems). The rule only handles a
clean two-table INNER equijoin; with 3+ tables (a second JOIN) it bails rather
than guess a covering index across the chain. Should NOT fire
`index.join_filter_missing_index`.
