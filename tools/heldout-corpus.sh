#!/usr/bin/env bash
# Fetch a held-out corpus of third-party production T-SQL and lint it.
#
# The eval corpus is hand-authored, so its F1 proves "no regression on the cases
# we wrote" and nothing about a real-world false-positive rate. This does the
# other half: real, widely-deployed T-SQL written by people who have never seen
# this linter. Nothing here is vendored into the repo — it is fetched from
# upstream, under those projects' own licences, and only ever read.
#
# Usage: ./tools/heldout-corpus.sh [outdir]
set -euo pipefail
OUT="${1:-target/heldout}"
BIN="./target/release/dbopt"
[ -x "$BIN" ] || { echo "build first: cargo build --workspace --release" >&2; exit 2; }
mkdir -p "$OUT"

fetch() { # url filename
  [ -s "$OUT/$2" ] || curl -fsSL --max-time 60 "$1" -o "$OUT/$2"
}
# Two distributions, deliberately. DBA tooling and application schemas fail in
# different ways: the tooling exercises dynamic SQL, DMV queries and table
# variables; the schemas exercise triggers, foreign keys, quoted identifiers and
# view definitions. Testing only one hides the other's false positives.
FRK=https://raw.githubusercontent.com/BrentOzarULTD/SQL-Server-First-Responder-Kit/main
MSS=https://raw.githubusercontent.com/microsoft/sql-server-samples/master/samples/databases
# --- DBA tooling ---
fetch https://raw.githubusercontent.com/olahallengren/sql-server-maintenance-solution/master/MaintenanceSolution.sql ola_maintenance.sql
fetch "$FRK/sp_Blitz.sql"       sp_blitz.sql
fetch "$FRK/sp_BlitzIndex.sql"  sp_blitzindex.sql
fetch "$FRK/sp_BlitzCache.sql"  sp_blitzcache.sql
# --- application schemas + business logic ---
fetch "$MSS/northwind-pubs/instnwnd.sql" northwind.sql
fetch "$MSS/northwind-pubs/instpubs.sql" pubs.sql
fetch "$MSS/adventure-works/oltp-install-script/instawdb.sql" adventureworks.sql
fetch https://raw.githubusercontent.com/lerocha/chinook-database/master/ChinookDatabase/DataSources/Chinook_SqlServer.sql chinook.sql

echo "corpus: $(cat "$OUT"/*.sql | wc -l) lines across $(ls "$OUT"/*.sql | wc -l) files"
"$BIN" lint "$OUT" --format json > "$OUT/findings.json" || true
python3 - "$OUT/findings.json" <<'PY'
import json, sys, collections
d = json.load(open(sys.argv[1]))
print("counts by severity:", d["countsBySeverity"])
c = collections.Counter(f["rule"] for f in d["findings"] if f["severity"] in ("critical", "error"))
print("\nhigh-severity findings (classify every one of these by hand):")
for r, n in c.most_common():
    print(f"  {n:>4}  {r}")
print(f"\ntotal high-severity: {sum(c.values())}")
PY
