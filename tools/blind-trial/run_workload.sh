#!/usr/bin/env bash
# Mixed workload against BlindTrial for $1 seconds (default 240). Output discarded.
set -u
T="$(cd "$(dirname "$0")" && pwd)"; source "$T/env.sh"
DUR=${1:-240}; END=$(( $(date +%s) + DUR ))
run() { "$SQLCMD" -S "$DBOPT_SERVER" -U "$DBOPT_USER" -P "$PW" -C -d BlindTrial -i "$1" "${@:2}" >/dev/null 2>>"$T/workload_errors.log"; }
# Prime the sniffing proc with the rare value so the cached plan is seek+lookup
run /dev/stdin <<<"EXEC sp_recompile 'dbo.Orders'; EXEC sp_recompile 'dbo.Customers'; EXEC sp_recompile 'dbo.Products'; EXEC sp_recompile 'dbo.Shipments'; EXEC sp_recompile 'dbo.Events'; EXEC sp_recompile 'dbo.OrderLines'; EXEC sp_recompile 'dbo.AuditLog'; EXEC dbo.usp_OrdersByStatus 'CANCELLED';"
loop_oltp() { local i=0; while [ $(date +%s) -lt $END ]; do i=$((i+1))
  run "$T/wl_oltp.sql" -v OID=$((RANDOM*30+1)) CID=$((RANDOM*6+1)) E7=$((RANDOM%7)) TRK=$(printf '%012d' $(( (RANDOM*9+1)*7 ))) CAT=$((RANDOM%20+1)) PID=$((RANDOM+1)) AID=$((RANDOM*3+1)); done; echo "oltp iterations=$i"; }
loop_report() { local i=0; local CHS=(WEB MOBILE STORE PARTNER); local STS=(PENDING RETURNED CANCELLED CANCELLED); while [ $(date +%s) -lt $END ]; do i=$((i+1))
  m=$((RANDOM%24+1)); y=$((2024 + (m+7)/12 )); mm=$(( (m+7)%12 + 1 ))
  run "$T/wl_report.sql" -v CH=${CHS[RANDOM%4]} D1=$(printf '%d-%02d-01' $y $mm) D2=$(printf '%d-%02d-08' $y $mm) TERM="Gizmo $((RANDOM%5000))" ST=${STS[RANDOM%4]}; done; echo "report iterations=$i"; }
loop_heavy() { local i=0; while [ $(date +%s) -lt $END ]; do i=$((i+1))
  run "$T/wl_heavy.sql" -v SINCE=2026-07-$((RANDOM%20+10)) PID=$((RANDOM%49000+1)); sleep 2; done; echo "heavy iterations=$i"; }
loop_oltp & loop_oltp & loop_report & loop_heavy &
wait
