#!/usr/bin/env bash
# Run the baseline (or the optimized) query against the user's SQL Server 2025
# container with STATISTICS TIME + IO on. Captures output for the case-study
# write-up.
#
# Usage:
#   SA_PASSWORD='<choose-a-strong-password>' ./05_run.sh baseline
#   SA_PASSWORD='<choose-a-strong-password>' ./05_run.sh optimized
#
# Env vars (with defaults):
#   DBOPT_CONTAINER (default: dbopt-sql2025)
#   SA_PASSWORD      (required)
#   DB_NAME          (default: dbopt_case)

set -euo pipefail

mode="${1:-baseline}"
container="${DBOPT_CONTAINER:-dbopt-sql2025}"
db="${DB_NAME:-dbopt_case}"
: "${SA_PASSWORD:?set SA_PASSWORD before running}"

case "$mode" in
    baseline)  proc="dbo.GetGmailSReport" ;;
    optimized) proc="dbo.GetGmailSReport_Fast" ;;
    *) echo "usage: $0 [baseline|optimized]"; exit 2 ;;
esac

ts="$(date +%Y%m%d-%H%M%S)"
out="runs/${mode}-${ts}.txt"
mkdir -p runs

echo "→ executing $proc on $container/$db"
echo "→ output: $out"

docker exec -i "$container" /opt/mssql-tools18/bin/sqlcmd \
    -S localhost,1433 -U sa -P "$SA_PASSWORD" -C -d "$db" \
    -Q "DBCC FREEPROCCACHE WITH NO_INFOMSGS;
        DBCC DROPCLEANBUFFERS WITH NO_INFOMSGS;
        SET STATISTICS TIME ON;
        SET STATISTICS IO ON;
        EXEC $proc;" \
    -W -h -1 -y 80 \
    > "$out" 2>&1

echo "→ done"
tail -30 "$out"
