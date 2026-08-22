# Connection for the blind-trial scripts. Source this; it reads the same env
# vars dbopt-sentinel uses so the password never appears on a command line
# you typed: export DBOPT_SERVER='localhost,1433' DBOPT_USER=sa DBOPT_PASSWORD='…'
: "${DBOPT_SERVER:=localhost,1433}"; : "${DBOPT_USER:=sa}"; : "${DBOPT_PASSWORD:?set DBOPT_PASSWORD}"
export PW="$DBOPT_PASSWORD"
SQLCMD="${SQLCMD:-/opt/mssql-tools18/bin/sqlcmd}"
sq()  { "$SQLCMD" -S "$DBOPT_SERVER" -U "$DBOPT_USER" -P "$PW" -C -d BlindTrial -W -b "$@"; }
sqm() { "$SQLCMD" -S "$DBOPT_SERVER" -U "$DBOPT_USER" -P "$PW" -C -d master -W -b "$@"; }
