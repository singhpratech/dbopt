Leading `%` in a LIKE pattern guarantees an index scan — the engine has no anchor to seek by. Should fire `sarg.leading_wildcard`. The fix is full-text search or a computed reverse-indexed column.
