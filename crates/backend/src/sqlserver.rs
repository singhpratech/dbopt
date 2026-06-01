use crate::routes::ConnectReq;
use analyzer_core::advisor_workload::QueryWorkloadStat;
use analyzer_core::dmv::{DmvBundle, IndexUsage, IndexMeta, MissingIndex, PartitionStats};
use std::time::Duration;
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

/// How long we'll wait for the initial TCP connect before giving up. Without
/// this the OS default (60–120s on an unreachable host) leaves the request — and
/// the user — hanging with no feedback.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Map a tiberius connect/login failure to a safe, actionable category message.
///
/// We never embed the raw error: the connection config carries the password
/// (`AuthMethod::sql_server`), and forwarding tiberius' `Display` verbatim into
/// an HTTP body / browser console / log risks leaking connection context. We
/// inspect the lowercased message to pick a useful category, but only ever emit
/// a fixed string the user can act on.
fn safe_connect_err(e: &tiberius::error::Error) -> anyhow::Error {
    let raw = e.to_string().to_ascii_lowercase();
    let msg = if raw.contains("login failed")
        || raw.contains("password")
        || raw.contains("authentication")
        || raw.contains("user")
    {
        "authentication failed — check the username and password"
    } else if raw.contains("cannot open database") || raw.contains("database") {
        "could not open the requested database — check the database name and that the login has access"
    } else if raw.contains("certificate") || raw.contains("tls") || raw.contains("ssl") {
        "TLS/certificate error — enable \"trust server certificate\" or install a trusted certificate"
    } else {
        "could not connect to SQL Server — check the server address and port, and that the instance is reachable"
    };
    anyhow::anyhow!(msg)
}

async fn open(req: &ConnectReq) -> anyhow::Result<Client<tokio_util::compat::Compat<TcpStream>>> {
    let mut config = Config::new();
    let parts: Vec<&str> = req.server.splitn(2, ',').collect();
    config.host(parts[0]);
    if let Some(port) = parts.get(1).and_then(|p| p.parse::<u16>().ok()) {
        config.port(port);
    } else {
        config.port(1433);
    }
    if let Some(db) = req.database.as_deref() { config.database(db); }
    apply_auth(&mut config, req)?;
    if req.trust_cert.unwrap_or(false) {
        config.trust_cert();
    }
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(config.get_addr()))
        .await
        .map_err(|_| anyhow::anyhow!("connection to SQL Server timed out after {}s", CONNECT_TIMEOUT.as_secs()))?
        .map_err(|_| anyhow::anyhow!("could not reach SQL Server — check the server address and port, and that the instance is reachable"))?;
    tcp.set_nodelay(true)?;
    let client = Client::connect(config, tcp.compat_write())
        .await
        .map_err(|e| safe_connect_err(&e))?;
    Ok(client)
}

/// Apply the requested authentication method to `config`. Three modes:
///   - `"sql"`        → SQL Server login (user + password). Works on every build.
///   - `"integrated"` → Windows integrated / current logged-in user (trusted
///                      connection, no password). Available on the Windows
///                      release build (winauth/SSPI) or a Linux build made with
///                      `--features integrated-auth` (Kerberos/GSSAPI ticket).
///   - `"windows"`    → an explicit Windows account: `DOMAIN\user` (or
///                      `user@domain`) + password, over NTLM. Windows builds only.
///
/// The mode defaults to `"sql"` when a username is supplied and `"integrated"`
/// otherwise, so older callers that omit `auth_mode` keep their prior behaviour.
/// A request for a Windows mode on a build that can't honor it returns a clear,
/// actionable error rather than a confusing transport failure.
fn apply_auth(config: &mut Config, req: &ConnectReq) -> anyhow::Result<()> {
    let has_user = req.user.as_deref().map(|u| !u.is_empty()).unwrap_or(false);
    let mode = req.auth_mode.as_deref().unwrap_or(if has_user { "sql" } else { "integrated" });
    match mode {
        "sql" => match (req.user.as_deref(), req.password.as_deref()) {
            (Some(u), Some(p)) if !u.is_empty() => { config.authentication(AuthMethod::sql_server(u, p)); }
            _ => anyhow::bail!("SQL authentication needs a login and a password."),
        },
        "windows" | "integrated" => {
            // We always compile `tiberius/winauth`; tiberius target-gates that
            // crate to `cfg(windows)`, so `AuthMethod::windows`/`Integrated`
            // exist exactly when we're building for Windows — hence `cfg(windows)`
            // (NOT a crate feature flag) is the correct gate here.
            #[cfg(windows)]
            {
                match (req.user.as_deref(), req.password.as_deref()) {
                    // Explicit account: DOMAIN\user (or user@domain) + password → NTLM.
                    (Some(u), Some(p)) if !u.is_empty() => { config.authentication(AuthMethod::windows(u, p)); }
                    // No credentials → current logged-in Windows user (SSPI / trusted connection).
                    _ => { config.authentication(AuthMethod::Integrated); }
                }
                return Ok(());
            }
            #[cfg(all(unix, feature = "integrated-auth"))]
            {
                // Kerberos/GSSAPI integrated auth uses the caller's existing
                // ticket; an explicit user/password is not applicable here.
                config.authentication(AuthMethod::Integrated);
                return Ok(());
            }
            #[cfg(not(any(windows, all(unix, feature = "integrated-auth"))))]
            {
                anyhow::bail!(
                    "Windows authentication isn't available in this build. The official dbopt build for Windows supports it natively; on Linux/macOS use SQL authentication (a login + password), or build with `--features integrated-auth` for Kerberos."
                );
            }
        }
        other => anyhow::bail!("unknown authentication mode '{other}' (expected sql, windows, or integrated)"),
    }
    Ok(())
}

pub async fn ping(req: &ConnectReq) -> anyhow::Result<String> {
    let mut client = open(req).await?;
    let row = client.query("SELECT @@VERSION", &[]).await?.into_row().await?;
    let s: Option<&str> = row.as_ref().and_then(|r| r.get(0));
    Ok(s.unwrap_or("unknown").to_string())
}

/// One programmable object pulled from `sys.sql_modules` joined to `sys.objects`.
/// `body` is the full `CREATE …` text (or what the engine stored after parsing —
/// no trailing GO, but otherwise the original definition).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DbModule {
    pub schema_name: String,
    pub object_name: String,
    pub object_type: String, // P / FN / IF / TF / V / TR
    pub body: String,
}

/// Enumerate every user-defined programmable object in the connected
/// database (stored procs, functions, views, triggers) and return their
/// source body. System objects are filtered. Encrypted modules return NULL
/// body and are skipped here.
pub async fn enumerate_modules(req: &ConnectReq) -> anyhow::Result<Vec<DbModule>> {
    let mut client = open(req).await?;
    let sql = "
        SELECT  s.name  AS schema_name,
                o.name  AS object_name,
                o.type  AS object_type,
                m.definition
        FROM sys.sql_modules AS m
        JOIN sys.objects     AS o ON o.object_id = m.object_id
        JOIN sys.schemas     AS s ON s.schema_id = o.schema_id
        WHERE o.is_ms_shipped = 0
          AND m.definition IS NOT NULL
        ORDER BY s.name, o.name;
    ";
    let stream = client.simple_query(sql).await?;
    let rows = stream.into_first_result().await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let schema = r.get::<&str, _>(0).unwrap_or("").to_string();
        let name = r.get::<&str, _>(1).unwrap_or("").to_string();
        let raw_type = r.get::<&str, _>(2).unwrap_or("").trim().to_string();
        let body = r.get::<&str, _>(3).unwrap_or("").to_string();
        out.push(DbModule { schema_name: schema, object_name: name, object_type: raw_type, body });
    }
    Ok(out)
}

/// List user databases on the connected server. System DBs (master/tempdb/
/// model/msdb) are excluded by `database_id > 4`. Ordered alphabetically.
/// One row of the live `sys.databases` list, with enough context for the UI to
/// group and gate the picker: system vs user, the lifecycle state, and whether
/// the *current login* can actually open it.
#[derive(serde::Serialize)]
pub struct DatabaseInfo {
    pub name: String,
    /// `database_id <= 4` — master/tempdb/model/msdb.
    pub system: bool,
    /// `state_desc`: ONLINE, RESTORING, RECOVERING, RECOVERY_PENDING, SUSPECT,
    /// EMERGENCY, OFFLINE, COPYING, OFFLINE_SECONDARY.
    pub state: String,
    /// `HAS_DBACCESS(name) = 1` — the connected login can USE this database.
    pub accessible: bool,
}

/// List EVERY database on the server — system and user, online or not.
///
/// Earlier this filtered `database_id > 4 AND state_desc = 'ONLINE'`, which
/// silently dropped the four system databases (a DBA legitimately wants `msdb`
/// for Agent/backup analysis) and made restoring/offline/suspect databases
/// vanish with no explanation. We now return them all with metadata so the UI
/// can group System vs User and disable the ones that can't be opened, instead
/// of hiding them. Ordered system-last, then alphabetical.
pub async fn list_databases(req: &ConnectReq) -> anyhow::Result<Vec<DatabaseInfo>> {
    let mut client = open(req).await?;
    let stream = client
        .simple_query(
            "SELECT name, \
                    CAST(database_id AS int) AS dbid, \
                    state_desc, \
                    CAST(CASE WHEN HAS_DBACCESS(name) = 1 THEN 1 ELSE 0 END AS int) AS has_access \
             FROM sys.databases \
             ORDER BY CASE WHEN database_id <= 4 THEN 1 ELSE 0 END, name",
        )
        .await?;
    let rows = stream.into_first_result().await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let name = match r.get::<&str, _>(0) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let dbid: i32 = r.get::<i32, _>(1).unwrap_or(0);
        let state = r.get::<&str, _>(2).unwrap_or("UNKNOWN").to_string();
        let has_access: i32 = r.get::<i32, _>(3).unwrap_or(0);
        out.push(DatabaseInfo {
            name,
            system: dbid <= 4,
            state,
            accessible: has_access == 1,
        });
    }
    Ok(out)
}

/// Fetch the estimated plan XML for the given T-SQL via SET SHOWPLAN_XML ON.
/// Returns one XML string per top-level statement, concatenated.
pub async fn estimated_plan(req: &ConnectReq, sql: &str) -> anyhow::Result<String> {
    let mut client = open(req).await?;
    // SHOWPLAN_XML returns the plan as the result set; the query itself is NOT executed.
    // The session option has to live in its own batch — wrap it inside sp_executesql.
    client.simple_query("SET SHOWPLAN_XML ON").await?;
    let stream = client.simple_query(sql).await?;
    let rows = stream.into_first_result().await.unwrap_or_default();
    let mut out = String::new();
    for r in rows {
        if let Some(xml) = r.get::<&str, _>(0) {
            out.push_str(xml);
            out.push('\n');
        }
    }
    // Best-effort: turn the option back off before the connection drops.
    let _ = client.simple_query("SET SHOWPLAN_XML OFF").await;
    if out.trim().is_empty() {
        return Err(anyhow::anyhow!("server returned no plan rows; check that the script is a valid SELECT/DML statement"));
    }
    Ok(out)
}

/// Statements we refuse to auto-run when capturing an ACTUAL plan. The rollback
/// transaction undoes INSERT/UPDATE/DELETE, but DDL, permission, server-control
/// and EXEC statements are either not safely transactional or can have effects
/// beyond this connection — and COMMIT would defeat the rollback. Conservative:
/// a whole-word, case-insensitive match anywhere in the batch (comments/strings
/// included). We'd rather over-block and have the user fall back to the estimate.
const ACTUAL_PLAN_BLOCKED: &[&str] = &[
    "DROP", "TRUNCATE", "ALTER", "CREATE", "GRANT", "REVOKE", "DENY", "COMMIT",
    "BACKUP", "RESTORE", "SHUTDOWN", "RECONFIGURE", "DBCC", "KILL", "WAITFOR",
    "EXEC", "EXECUTE",
];

/// First blocked keyword found in `sql` (whole-word, case-insensitive), if any.
/// Tokenizes on non-identifier characters so `sp_executesql` does NOT trip
/// `EXEC`, while a leading `EXEC sp_...` does.
fn actual_plan_blocked_keyword(sql: &str) -> Option<&'static str> {
    use std::collections::HashSet;
    let words: HashSet<String> = sql
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_uppercase())
        .collect();
    ACTUAL_PLAN_BLOCKED.iter().copied().find(|kw| words.contains(*kw))
}

/// Capture the ACTUAL execution plan (real row counts + runtime) by executing
/// the batch with `SET STATISTICS XML ON`, wrapped in a transaction we ALWAYS
/// roll back so DML leaves no trace. Destructive/DDL/EXEC batches are refused up
/// front. Returns the concatenated ShowPlanXML for every statement that produced
/// one. NOTE: this genuinely runs the query — it can take real time and pulls
/// result rows over the wire; the UI gates it behind an explicit action.
pub async fn actual_plan(req: &ConnectReq, sql: &str) -> anyhow::Result<String> {
    if let Some(kw) = actual_plan_blocked_keyword(sql) {
        return Err(anyhow::anyhow!(
            "Actual plan refused: the batch contains `{kw}`. The auto-rollback guard only runs SELECT/DML (and undoes any data changes); DDL, EXEC and server-control statements are blocked. Use ESTIMATED PLAN — it never executes the query."
        ));
    }

    let mut client = open(req).await?;
    // STATISTICS XML emits, after each executed statement, an extra single-column
    // result set holding that statement's <ShowPlanXML>.
    let _ = client.simple_query("SET STATISTICS XML ON").await;
    let _ = client.simple_query("BEGIN TRANSACTION").await;

    // Run + fully drain the batch BEFORE the rollback (the stream borrows client).
    // Capture the outcome so we roll back even when the query itself errors.
    let exec = match client.simple_query(sql).await {
        Ok(stream) => stream.into_results().await,
        Err(e) => Err(e),
    };

    // Undo everything regardless of success, then clear the session flag.
    let _ = client.simple_query("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await;
    let _ = client.simple_query("SET STATISTICS XML OFF").await;

    let result_sets = exec.map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let mut out = String::new();
    for rs in &result_sets {
        for row in rs {
            // The plan rows are a single string column; data columns of other
            // types fail the &str get and are skipped (never panics).
            if let Ok(Some(s)) = row.try_get::<&str, _>(0) {
                if s.contains("<ShowPlanXML") {
                    out.push_str(s);
                    out.push('\n');
                }
            }
        }
    }

    if out.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "the batch ran (and was rolled back) but returned no actual plan — it may produce no executable statement, or STATISTICS XML is unavailable on this server"
        ));
    }
    Ok(out)
}

/// Query Store config for the connected database. `capture_mode` is AUTO (skip
/// one-off/cheap queries — the engine default), ALL (capture every query), or
/// NONE/OFF. `enabled` is false when Query Store is OFF for the database.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryStoreStatus {
    pub enabled: bool,
    pub state: String,
    pub capture_mode: String,
    /// Whether the connected login can actually change the capture mode, i.e.
    /// holds ALTER on this database (db_owner / sysadmin). The UI gates the
    /// toggle on this so it never offers a change the login can't make.
    pub can_alter: bool,
}

pub async fn query_store_status(req: &ConnectReq) -> anyhow::Result<QueryStoreStatus> {
    let mut client = open(req).await?;

    // Does this login hold ALTER on the database? HAS_PERMS_BY_NAME accounts for
    // role membership (db_owner) and server roles (sysadmin), so it's the true
    // effective check. Run it independently so it works even when QS is OFF.
    let mut can_alter = false;
    if let Ok(s) = client
        .simple_query("SELECT CAST(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'ALTER') AS INT)")
        .await
    {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                can_alter = r.get::<i32, _>(0).unwrap_or(0) == 1;
            }
        }
    }

    let stream = client
        .simple_query("SELECT actual_state_desc, query_capture_mode_desc FROM sys.database_query_store_options")
        .await?;
    let rows = stream.into_first_result().await?;
    if let Some(r) = rows.into_iter().next() {
        let state = r.get::<&str, _>(0).unwrap_or("OFF").to_string();
        let mode = r.get::<&str, _>(1).unwrap_or("NONE").to_string();
        Ok(QueryStoreStatus { enabled: !state.eq_ignore_ascii_case("OFF"), state, capture_mode: mode, can_alter })
    } else {
        Ok(QueryStoreStatus { enabled: false, state: "OFF".into(), capture_mode: "NONE".into(), can_alter })
    }
}

/// Set the connected database's Query Store capture mode. `mode` is validated
/// against a strict allowlist — we NEVER interpolate arbitrary text into DDL —
/// and we target `DATABASE CURRENT` (the connected DB) so there is no database
/// name to escape. Caller (UI) shows this exact statement and requires explicit
/// confirmation before invoking — this is a Safe-Apply action, not auto-DDL.
pub async fn set_query_store_capture(req: &ConnectReq, mode: &str) -> anyhow::Result<String> {
    let mode = match mode.to_ascii_uppercase().as_str() {
        "AUTO" => "AUTO",
        "ALL" => "ALL",
        "NONE" => "NONE",
        other => return Err(anyhow::anyhow!("unsupported capture mode '{other}' (expected AUTO, ALL, or NONE)")),
    };
    if req.database.as_deref().map(|d| d.is_empty()).unwrap_or(true) {
        return Err(anyhow::anyhow!("select a database first — Query Store capture mode is per-database"));
    }
    let mut client = open(req).await?;
    // Query Store must be ON before its capture mode can be set; ON when already
    // on is a no-op. CURRENT = the connected database (no name interpolation).
    let _ = client.simple_query("ALTER DATABASE CURRENT SET QUERY_STORE = ON").await;
    let stmt = format!("ALTER DATABASE CURRENT SET QUERY_STORE (QUERY_CAPTURE_MODE = {mode})");
    client.simple_query(stmt.as_str()).await?;
    Ok(format!("Query Store capture mode set to {mode}"))
}

/// One Query Store query aggregated across its plans/intervals, ranked by
/// average duration. Pure telemetry — no execution, no user-table rows.
#[derive(Debug, serde::Serialize)]
pub struct QueryStoreTopQuery {
    pub query_id: i64,
    pub sql_text: String,
    pub executions: i64,
    pub avg_duration_ms: f64,
    pub max_duration_ms: f64,
    pub avg_cpu_ms: f64,
    pub avg_logical_reads: i64,
}

/// Read the connected database's top long-running queries from Query Store,
/// ranked by average duration. READ-ONLY: it queries the `sys.query_store_*`
/// catalog views only — it never executes the captured queries and never reads
/// rows from user tables. `limit` is clamped to an integer and interpolated into
/// `TOP (n)` (never user text), so there is nothing to escape. Durations are
/// converted from microseconds to milliseconds and weighted by execution count.
pub async fn query_store_top_queries(
    req: &ConnectReq,
    limit: u32,
) -> anyhow::Result<Vec<QueryStoreTopQuery>> {
    let limit = limit.clamp(1, 200);
    let mut client = open(req).await?;
    let sql = format!(
        "SELECT TOP ({limit}) m.query_id, m.execs, m.avg_ms, m.max_ms, m.avg_cpu_ms, m.avg_reads,
                CAST(LEFT(qt.query_sql_text, 2000) AS NVARCHAR(2000)) AS sql_text
         FROM (
           SELECT q.query_id,
                  CAST(SUM(rs.count_executions) AS BIGINT) AS execs,
                  CAST(SUM(rs.avg_duration * rs.count_executions) / NULLIF(SUM(rs.count_executions),0) / 1000.0 AS FLOAT) AS avg_ms,
                  CAST(MAX(rs.max_duration) / 1000.0 AS FLOAT) AS max_ms,
                  CAST(SUM(rs.avg_cpu_time * rs.count_executions) / NULLIF(SUM(rs.count_executions),0) / 1000.0 AS FLOAT) AS avg_cpu_ms,
                  CAST(SUM(rs.avg_logical_io_reads * rs.count_executions) / NULLIF(SUM(rs.count_executions),0) AS BIGINT) AS avg_reads,
                  MIN(q.query_text_id) AS query_text_id
           FROM sys.query_store_runtime_stats rs
           JOIN sys.query_store_plan p ON p.plan_id = rs.plan_id
           JOIN sys.query_store_query q ON q.query_id = p.query_id
           GROUP BY q.query_id
         ) m
         JOIN sys.query_store_query_text qt ON qt.query_text_id = m.query_text_id
         ORDER BY m.avg_ms DESC"
    );
    let stream = client.simple_query(sql.as_str()).await?;
    let rows = stream.into_first_result().await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(QueryStoreTopQuery {
            query_id: r.get::<i64, _>("query_id").unwrap_or(0),
            executions: r.get::<i64, _>("execs").unwrap_or(0),
            avg_duration_ms: r.get::<f64, _>("avg_ms").unwrap_or(0.0),
            max_duration_ms: r.get::<f64, _>("max_ms").unwrap_or(0.0),
            avg_cpu_ms: r.get::<f64, _>("avg_cpu_ms").unwrap_or(0.0),
            avg_logical_reads: r.get::<i64, _>("avg_reads").unwrap_or(0),
            sql_text: r.get::<&str, _>("sql_text").unwrap_or("").to_string(),
        });
    }
    Ok(out)
}

/// True when `text` references the bare identifier `ident` as a whole word
/// (case-insensitively). We implement the word boundary BY HAND — no regex
/// crate — so "Order" does NOT match the longer token "Orders": the character
/// immediately before and after each candidate position must be a non-word
/// character (not ASCII-alphanumeric and not `_`). This lets us match `Orders`,
/// `[Orders]`, and `dbo.Orders` (the `[`, `]`, `.` and whitespace around the
/// token are all word boundaries) without false-matching substrings.
fn references_table_token(text: &str, ident: &str) -> bool {
    if ident.is_empty() {
        return false;
    }
    let hay = text.as_bytes();
    let needle = ident.as_bytes();
    let nlen = needle.len();
    if hay.len() < nlen {
        return false;
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0usize;
    while i + nlen <= hay.len() {
        // Case-insensitive compare of the candidate window against the needle.
        let matches = hay[i..i + nlen]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b));
        if matches {
            let before_ok = i == 0 || !is_word(hay[i - 1]);
            let after_idx = i + nlen;
            let after_ok = after_idx >= hay.len() || !is_word(hay[after_idx]);
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Derive per-(schema, table) workload frequency from Query Store so the
/// CONNECTED index advisor can ground its recommendations in HOW OFTEN the
/// benefiting query actually runs (see `advisor_workload`).
///
/// READ-ONLY: queries only the `sys.query_store_*` catalog views — it never
/// executes captured queries and never reads user-table rows. Returns `[]` when
/// Query Store is disabled or unreadable; callers MUST degrade gracefully.
///
/// Heuristic (documented): the missing-index DMV groups by target object, not
/// by query, so we credit each real user table — taken from the authoritative
/// index metadata we already pulled — with the MAX single-query execution count
/// among Query Store queries whose text references that table by a
/// case-insensitive, word-boundary match on the bare table identifier. We emit
/// the index metadata's own schema+table so `busiest_for` (which requires BOTH
/// to match) lines up. Unmatched tables are skipped — no fake zero rows.
pub async fn query_store_workload(
    req: &ConnectReq,
    indexes: &[IndexMeta],
) -> anyhow::Result<Vec<QueryWorkloadStat>> {
    let mut client = open(req).await?;

    // Capture-window length in hours from the runtime-stats intervals. Falls
    // back to 24h when the view is empty / null / unreadable so a single day's
    // capture reads naturally and we never divide by zero downstream.
    let mut window_hours = 24.0_f64;
    if let Ok(s) = client
        .simple_query(
            "SELECT CAST(DATEDIFF(MINUTE, MIN(start_time), MAX(end_time)) / 60.0 AS FLOAT) \
             FROM sys.query_store_runtime_stats_interval",
        )
        .await
    {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(w) = rows.into_iter().next().and_then(|r| r.get::<f64, _>(0)) {
                if w > 0.0 {
                    window_hours = w;
                }
            }
        }
    }

    // Per-query total executions + text, ranked by FREQUENCY (not duration),
    // grouped by query_id exactly like the top-queries query. TOP 200 keeps the
    // word-boundary scan bounded. 2014+ portable: CAST to BIGINT, NULLIF guards.
    let sql = "SELECT TOP (200) m.execs, \
                CAST(LEFT(qt.query_sql_text, 2000) AS NVARCHAR(2000)) AS sql_text \
         FROM ( \
           SELECT q.query_id, \
                  CAST(SUM(rs.count_executions) AS BIGINT) AS execs, \
                  MIN(q.query_text_id) AS query_text_id \
           FROM sys.query_store_runtime_stats rs \
           JOIN sys.query_store_plan p ON p.plan_id = rs.plan_id \
           JOIN sys.query_store_query q ON q.query_id = p.query_id \
           GROUP BY q.query_id \
         ) m \
         JOIN sys.query_store_query_text qt ON qt.query_text_id = m.query_text_id \
         ORDER BY m.execs DESC";
    let stream = client.simple_query(sql).await?;
    let rows = stream.into_first_result().await?;

    // (execs, sql_text) for each captured query, freshly owned so we can scan
    // each query's text against every distinct table identifier below.
    let queries: Vec<(u64, String)> = rows
        .iter()
        .map(|r| {
            (
                r.get::<i64, _>("execs").unwrap_or(0).max(0) as u64,
                r.get::<&str, _>("sql_text").unwrap_or("").to_string(),
            )
        })
        .collect();

    // Distinct (schema, table) set from the real user tables we have metadata
    // for. Preserve the metadata's authoritative casing for the emitted stat.
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for idx in indexes {
        let key = (
            idx.schema_name.to_ascii_lowercase(),
            idx.table_name.to_ascii_lowercase(),
        );
        if !seen.insert(key) {
            continue;
        }
        // Max single-query execution count among QS queries that reference this
        // table by a word-boundary match on the bare table identifier.
        let mut max_execs = 0u64;
        let mut matched = false;
        for (execs, text) in &queries {
            if references_table_token(text, &idx.table_name) {
                matched = true;
                if *execs > max_execs {
                    max_execs = *execs;
                }
            }
        }
        if matched {
            out.push(QueryWorkloadStat {
                schema_name: idx.schema_name.clone(),
                table_name: idx.table_name.clone(),
                execution_count: max_execs,
                window_hours,
            });
        }
    }

    Ok(out)
}

/// One T-SQL syntax diagnostic from the real engine parser. `number` is the
/// SQL Server error number (e.g. 102 "Incorrect syntax near …"), `line` is the
/// 1-based line within the submitted batch.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParseDiagnostic {
    pub number: u32,
    pub line: u32,
    pub message: String,
}

/// Validate a T-SQL batch against the REAL SQL Server parser via `SET PARSEONLY
/// ON` — the same check SSMS's "Parse" (Ctrl+F5) runs. This verifies syntax and
/// keyword correctness for the connected server's exact version WITHOUT
/// executing anything and WITHOUT binding object names (so a missing table does
/// NOT fail here — only genuine syntax/keyword errors do). An empty Vec means
/// the batch parses cleanly. We open a fresh connection and drop it after, so
/// the lingering PARSEONLY state (which can't turn itself off while ON) is moot.
pub async fn parse_check(req: &ConnectReq, sql: &str) -> anyhow::Result<Vec<ParseDiagnostic>> {
    let mut client = open(req).await?;
    // PARSEONLY lives in its own batch; SET options persist for the session.
    let _ = client.simple_query("SET PARSEONLY ON").await;
    let result: Result<(), tiberius::error::Error> = async {
        let stream = client.simple_query(sql).await?;
        // Drain so the server flushes every token — a syntax error surfaces here.
        stream.into_results().await?;
        Ok(())
    }
    .await;

    match result {
        Ok(()) => Ok(Vec::new()),
        Err(tiberius::error::Error::Server(e)) => Ok(vec![ParseDiagnostic {
            number: e.code(),
            line: e.line(),
            message: e.message().to_string(),
        }]),
        // A transport/protocol failure is a real error, not a syntax verdict.
        Err(other) => Err(anyhow::anyhow!(other.to_string())),
    }
}

/// Operational-health facts for the connected instance + database. Every field
/// is `Option` because each is gathered best-effort: a feature may not exist on
/// the target version (e.g. `sys.dm_db_log_info` is 2016+), or the login may
/// lack access (e.g. msdb for backup history, or on Azure SQL DB where msdb
/// isn't exposed). A missing fact simply yields no check, never an error.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct OperationalFacts {
    pub cpu_count: Option<i64>,
    pub maxdop: Option<i64>,
    pub cost_threshold: Option<i64>,
    pub optimize_for_adhoc: Option<bool>,
    pub recovery_model: Option<String>,
    pub auto_shrink: Option<bool>,
    pub auto_close: Option<bool>,
    pub page_verify: Option<String>,
    pub auto_create_stats: Option<bool>,
    pub auto_update_stats: Option<bool>,
    pub vlf_count: Option<i64>,
    /// True only when the msdb backup history query actually succeeded. This is
    /// the honesty guard: a `None` age with `backups_readable == false` means
    /// "we couldn't read msdb" (no access / Azure SQL DB) — NOT "no backup
    /// exists". We only ever warn about a missing backup when this is `true`.
    pub backups_readable: bool,
    pub last_full_backup_age_hours: Option<i64>,
    pub last_log_backup_age_hours: Option<i64>,

    // --- DBCC CHECKDB integrity (per connected database) -------------------
    /// True only when DBCC DBINFO actually returned a parseable
    /// dbi_dbccLastKnownGood row. Like `backups_readable`, this is the honesty
    /// guard: we only ever emit a "stale / never run" integrity check when this
    /// is `true`, so a permission/parse gap can never masquerade as a finding.
    pub checkdb_readable: bool,
    /// Whole days since the last successful DBCC CHECKDB for the connected
    /// database. `None` while `checkdb_readable` means the 1900-01-01 sentinel
    /// (or a NULL marker) was read → integrity check has NEVER run.
    pub checkdb_last_good_age_days: Option<i64>,
    /// READ_ONLY databases never update the CHECKDB marker, so a "stale" verdict
    /// would be a false alarm. We suppress the check when this is `true`.
    pub db_is_read_only: Option<bool>,

    // --- High-availability replica health (AG only) ------------------------
    /// True only when sys.dm_hadr_database_replica_states was readable. An empty
    /// result (server not in an AG) is NOT a failure — `hadr_replicas` is simply
    /// empty and no HADR check is emitted.
    pub hadr_readable: bool,
    /// One row per (replica, database) where HADR is configured. Empty when the
    /// instance is not in an Availability Group.
    pub hadr_replicas: Vec<HadrReplicaFact>,

    // --- Scheduled-maintenance (Agent) job failures ------------------------
    /// True only when msdb job history was readable. A permission gap leaves
    /// this `false` so "no failures" can never masquerade as health.
    pub jobs_readable: bool,
    /// Enabled jobs whose outcome row failed within the lookback window.
    pub failed_jobs: Vec<FailedJobFact>,

    // --- Instant File Initialization (instance-wide) -----------------------
    /// `Some(true)`/`Some(false)` only when sys.dm_server_services exposed the
    /// `instant_file_initialization_enabled` column (SQL Server 2016 SP1+ /
    /// 2012 SP4+). `None` on older builds (column absent) → no check.
    pub ifi_enabled: Option<bool>,

    // --- tempdb data-file count vs cores -----------------------------------
    /// Count of tempdb ROWS (data) files from sys.master_files.
    pub tempdb_data_files: Option<i64>,
    /// True when the tempdb data files are NOT all the same size (the round-robin
    /// allocator is defeated by unequal sizes). `None` when unreadable / single
    /// file.
    pub tempdb_files_unequal: Option<bool>,

    // --- Dangerous global trace flags --------------------------------------
    /// True only when DBCC TRACESTATUS(-1) was readable. An empty global-flag
    /// result is NOT a failure — `global_trace_flags` is simply empty.
    pub trace_flags_readable: bool,
    /// Globally-enabled trace-flag numbers (Global = 1).
    pub global_trace_flags: Vec<i64>,
}

/// One (replica, database) row from `sys.dm_hadr_database_replica_states`,
/// joined to the replica/group names. All best-effort: every field is read
/// defensively, and the whole vec is empty when the instance is not in an AG.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct HadrReplicaFact {
    pub ag_name: String,
    pub replica_server_name: String,
    pub database_name: String,
    /// NOT SYNCHRONIZING / SYNCHRONIZING / SYNCHRONIZED.
    pub synchronization_state: String,
    /// NOT_HEALTHY / PARTIALLY_HEALTHY / HEALTHY.
    pub synchronization_health: String,
    /// `availability_mode_desc`: SYNCHRONOUS_COMMIT / ASYNCHRONOUS_COMMIT.
    pub availability_mode: String,
    pub is_suspended: bool,
    pub suspend_reason: Option<String>,
    pub redo_queue_size_kb: Option<i64>,
    pub redo_rate_kb: Option<i64>,
}

/// One failed Agent-job outcome row from msdb job history within the lookback.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FailedJobFact {
    pub job_name: String,
    /// Most-recent failure timestamp, formatted by `msdb.dbo.agent_datetime`.
    pub run_at: Option<String>,
    /// Count of failed outcome rows for this job within the lookback window.
    pub failure_count: i64,
    /// The latest failure message (truncated server-side).
    pub message: String,
}

/// Gather operational-health facts (server config, current-DB settings, log VLF
/// count, backup ages). Each probe is independent and best-effort so one
/// unsupported/denied query never sinks the rest.
pub async fn pull_operational(req: &ConnectReq) -> anyhow::Result<OperationalFacts> {
    let mut client = open(req).await?;
    let mut f = OperationalFacts::default();

    // Server scheduler count (for MAXDOP advice). int -> bigint.
    if let Ok(s) = client.simple_query("SELECT CAST(cpu_count AS BIGINT) FROM sys.dm_os_sys_info").await {
        if let Ok(rows) = s.into_first_result().await {
            f.cpu_count = rows.first().and_then(|r| r.get::<i64, _>(0));
        }
    }

    // Parallelism + plan-cache config, pivoted by name. value_in_use is
    // sql_variant, so CAST to BIGINT for a stable type.
    let q_cfg = "SELECT \
        MAX(CASE WHEN name='max degree of parallelism' THEN CAST(value_in_use AS BIGINT) END), \
        MAX(CASE WHEN name='cost threshold for parallelism' THEN CAST(value_in_use AS BIGINT) END), \
        MAX(CASE WHEN name='optimize for ad hoc workloads' THEN CAST(value_in_use AS BIGINT) END) \
        FROM sys.configurations";
    if let Ok(s) = client.simple_query(q_cfg).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.first() {
                f.maxdop = r.get::<i64, _>(0);
                f.cost_threshold = r.get::<i64, _>(1);
                f.optimize_for_adhoc = r.get::<i64, _>(2).map(|v| v != 0);
            }
        }
    }

    // Current-database settings.
    let q_db = "SELECT recovery_model_desc, \
        CAST(is_auto_shrink_on AS BIT), CAST(is_auto_close_on AS BIT), \
        page_verify_option_desc, \
        CAST(is_auto_create_stats_on AS BIT), CAST(is_auto_update_stats_on AS BIT) \
        FROM sys.databases WHERE database_id = DB_ID()";
    if let Ok(s) = client.simple_query(q_db).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.first() {
                f.recovery_model = r.get::<&str, _>(0).map(|s| s.to_string());
                f.auto_shrink = r.get::<bool, _>(1);
                f.auto_close = r.get::<bool, _>(2);
                f.page_verify = r.get::<&str, _>(3).map(|s| s.to_string());
                f.auto_create_stats = r.get::<bool, _>(4);
                f.auto_update_stats = r.get::<bool, _>(5);
            }
        }
    }

    // Transaction-log VLF count (sys.dm_db_log_info is 2016+).
    if let Ok(s) = client.simple_query("SELECT CAST(COUNT(*) AS BIGINT) FROM sys.dm_db_log_info(DB_ID())").await {
        if let Ok(rows) = s.into_first_result().await {
            f.vlf_count = rows.first().and_then(|r| r.get::<i64, _>(0));
        }
    }

    // Backup ages from msdb (best-effort; absent on Azure SQL DB / restricted logins).
    let q_bak = "SELECT \
        CAST(DATEDIFF(HOUR, MAX(CASE WHEN type='D' THEN backup_finish_date END), GETDATE()) AS BIGINT), \
        CAST(DATEDIFF(HOUR, MAX(CASE WHEN type='L' THEN backup_finish_date END), GETDATE()) AS BIGINT) \
        FROM msdb.dbo.backupset WHERE database_name = DB_NAME()";
    if let Ok(s) = client.simple_query(q_bak).await {
        if let Ok(rows) = s.into_first_result().await {
            // The query ran → backup history is genuinely readable. A NULL age
            // now honestly means "no such backup", not "couldn't look".
            f.backups_readable = true;
            if let Some(r) = rows.first() {
                f.last_full_backup_age_hours = r.get::<i64, _>(0);
                f.last_log_backup_age_hours = r.get::<i64, _>(1);
            }
        }
    }

    // Whether the connected DB is READ_ONLY (suppresses a false "stale CHECKDB"
    // alarm — the integrity marker never advances on a read-only database).
    if let Ok(s) = client
        .simple_query("SELECT CAST(is_read_only AS BIT) FROM sys.databases WHERE database_id = DB_ID()")
        .await
    {
        if let Ok(rows) = s.into_first_result().await {
            f.db_is_read_only = rows.first().and_then(|r| r.get::<bool, _>(0));
        }
    }

    // Last successful DBCC CHECKDB for the connected database. DBCC has no FROM,
    // so we capture DBINFO into a temp table and pivot the dbi_dbccLastKnownGood
    // field. The 1900-01-01 sentinel (or a NULL value) means CHECKDB has never
    // succeeded → `checkdb_last_good_age_days = None` while `checkdb_readable`.
    // READ-ONLY: DBCC DBINFO only reads the boot page; it never touches data.
    let q_checkdb = "SET NOCOUNT ON; \
        DECLARE @dbinfo TABLE (ParentObject NVARCHAR(255), Object NVARCHAR(255), Field NVARCHAR(255), Value NVARCHAR(255)); \
        INSERT INTO @dbinfo EXEC ('DBCC DBINFO() WITH TABLERESULTS, NO_INFOMSGS'); \
        DECLARE @lkg DATETIME = (SELECT TRY_CONVERT(DATETIME, MAX(Value)) FROM @dbinfo WHERE Field = 'dbi_dbccLastKnownGood'); \
        SELECT CASE WHEN @lkg IS NULL OR @lkg <= '1900-01-01' THEN NULL \
                    ELSE CAST(DATEDIFF(DAY, @lkg, GETDATE()) AS BIGINT) END AS age_days";
    if let Ok(s) = client.simple_query(q_checkdb).await {
        if let Ok(rows) = s.into_first_result().await {
            // The batch ran → the integrity marker is genuinely readable. A NULL
            // age now honestly means "never run", not "couldn't look".
            f.checkdb_readable = true;
            f.checkdb_last_good_age_days = rows.first().and_then(|r| r.get::<i64, _>(0));
        }
    }

    // High-availability replica health. These DMVs return rows ONLY where an
    // Availability Group is configured; an empty result is not a failure (we
    // mirror the DMV-empty-is-not-broken rule). The whole probe is wrapped so an
    // older build (DMVs absent pre-2012) or a permission gap leaves it unread.
    let q_hadr = "SELECT ag.name AS ag_name, ar.replica_server_name, drs.database_name, \
               drs.synchronization_state_desc, drs.synchronization_health_desc, \
               ar.availability_mode_desc, CAST(drs.is_suspended AS BIT) AS is_suspended, \
               drs.suspend_reason_desc, \
               CAST(drs.redo_queue_size AS BIGINT) AS redo_queue_size, \
               CAST(drs.redo_rate AS BIGINT) AS redo_rate \
        FROM sys.dm_hadr_database_replica_states AS drs \
        JOIN sys.availability_replicas AS ar ON drs.replica_id = ar.replica_id \
        JOIN sys.availability_groups   AS ag ON ar.group_id   = ag.group_id";
    if let Ok(s) = client.simple_query(q_hadr).await {
        if let Ok(rows) = s.into_first_result().await {
            f.hadr_readable = true;
            for r in rows {
                f.hadr_replicas.push(HadrReplicaFact {
                    ag_name: r.get::<&str, _>(0).unwrap_or("").to_string(),
                    replica_server_name: r.get::<&str, _>(1).unwrap_or("").to_string(),
                    database_name: r.get::<&str, _>(2).unwrap_or("").to_string(),
                    synchronization_state: r.get::<&str, _>(3).unwrap_or("").to_string(),
                    synchronization_health: r.get::<&str, _>(4).unwrap_or("").to_string(),
                    availability_mode: r.get::<&str, _>(5).unwrap_or("").to_string(),
                    is_suspended: r.get::<bool, _>(6).unwrap_or(false),
                    suspend_reason: r.get::<&str, _>(7).map(|s| s.to_string()),
                    redo_queue_size_kb: r.get::<i64, _>(8),
                    redo_rate_kb: r.get::<i64, _>(9),
                });
            }
        }
    }

    // Failed scheduled-maintenance jobs (msdb Agent history) within a 30-day
    // lookback. step_id = 0 is the job-outcome row (not an individual step);
    // run_status = 0 is Failed. Best-effort: unreadable msdb (Azure SQL DB /
    // restricted login) leaves `jobs_readable = false` so a permission gap can
    // never read as "no failures".
    let q_jobs = "SELECT j.name AS job_name, COUNT(*) AS failure_count, \
               MAX(msdb.dbo.agent_datetime(h.run_date, h.run_time)) AS last_run_at, \
               CAST(MAX(LEFT(h.message, 500)) AS NVARCHAR(500)) AS last_message \
        FROM msdb.dbo.sysjobhistory AS h \
        JOIN msdb.dbo.sysjobs       AS j ON h.job_id = j.job_id \
        WHERE h.run_status = 0 AND h.step_id = 0 AND j.enabled = 1 \
          AND msdb.dbo.agent_datetime(h.run_date, h.run_time) >= DATEADD(DAY, -30, GETDATE()) \
        GROUP BY j.name \
        ORDER BY last_run_at DESC";
    if let Ok(s) = client.simple_query(q_jobs).await {
        if let Ok(rows) = s.into_first_result().await {
            f.jobs_readable = true;
            for r in rows {
                f.failed_jobs.push(FailedJobFact {
                    job_name: r.get::<&str, _>(0).unwrap_or("").to_string(),
                    failure_count: r.get::<i64, _>(1).unwrap_or(0),
                    run_at: r.get::<&str, _>(2).map(|s| s.to_string()),
                    message: r.get::<&str, _>(3).unwrap_or("").to_string(),
                });
            }
        }
    }

    // Instant File Initialization (instance-wide). VERSION-GATE: the
    // `instant_file_initialization_enabled` column exists only on SQL Server
    // 2016 SP1+ / 2012 SP4+. On older builds the query fails and `ifi_enabled`
    // stays None → no check (the same honest pattern as sys.dm_db_log_info).
    let q_ifi = "SELECT CAST(CASE WHEN UPPER(LTRIM(RTRIM(instant_file_initialization_enabled))) = 'Y' \
               THEN 1 ELSE 0 END AS BIT) \
        FROM sys.dm_server_services WHERE filename LIKE '%sqlservr.exe%'";
    if let Ok(s) = client.simple_query(q_ifi).await {
        if let Ok(rows) = s.into_first_result().await {
            f.ifi_enabled = rows.first().and_then(|r| r.get::<bool, _>(0));
        }
    }

    // tempdb data-file count + whether the files are equally sized. Recommended
    // count is min(cores, 8); unequal sizes defeat the round-robin allocator.
    let q_tempdb = "SELECT CAST(COUNT(*) AS BIGINT) AS file_count, \
               CAST(CASE WHEN MIN(size) = MAX(size) THEN 0 ELSE 1 END AS BIT) AS unequal \
        FROM sys.master_files WHERE database_id = 2 AND type_desc = 'ROWS'";
    if let Ok(s) = client.simple_query(q_tempdb).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.first() {
                f.tempdb_data_files = r.get::<i64, _>(0);
                f.tempdb_files_unequal = r.get::<bool, _>(1);
            }
        }
    }

    // Globally-enabled trace flags. DBCC has no FROM, so capture TRACESTATUS(-1)
    // into a temp table and keep only Global = 1. An empty result is NOT a
    // failure — `trace_flags_readable` is true with an empty `global_trace_flags`.
    let q_tf = "SET NOCOUNT ON; \
        DECLARE @ts TABLE (TraceFlag INT, Status INT, Global INT, [Session] INT); \
        INSERT INTO @ts EXEC ('DBCC TRACESTATUS(-1) WITH NO_INFOMSGS'); \
        SELECT CAST(TraceFlag AS BIGINT) FROM @ts WHERE Global = 1";
    if let Ok(s) = client.simple_query(q_tf).await {
        if let Ok(rows) = s.into_first_result().await {
            f.trace_flags_readable = true;
            for r in rows {
                if let Some(tf) = r.get::<i64, _>(0) {
                    f.global_trace_flags.push(tf);
                }
            }
        }
    }

    Ok(f)
}

pub async fn pull_dmv_bundle(req: &ConnectReq) -> anyhow::Result<DmvBundle> {
    let mut client = open(req).await?;

    let mut bundle = DmvBundle::default();

    // Index usage stats
    let q_usage = r#"
        SELECT DB_NAME() AS database_name,
               s.name AS schema_name,
               t.name AS table_name,
               COALESCE(i.name, '(heap)') AS index_name,
               ISNULL(u.user_seeks, 0) AS user_seeks,
               ISNULL(u.user_scans, 0) AS user_scans,
               ISNULL(u.user_lookups, 0) AS user_lookups,
               ISNULL(u.user_updates, 0) AS user_updates
        FROM sys.indexes i
        JOIN sys.tables t ON t.object_id = i.object_id
        JOIN sys.schemas s ON s.schema_id = t.schema_id
        LEFT JOIN sys.dm_db_index_usage_stats u
          ON u.object_id = i.object_id AND u.index_id = i.index_id AND u.database_id = DB_ID()
        WHERE t.is_ms_shipped = 0
    "#;
    if let Ok(stream) = client.simple_query(q_usage).await {
        let rows = stream.into_first_result().await.unwrap_or_default();
        for r in rows {
            bundle.index_usage.push(IndexUsage {
                database_name: r.get::<&str, _>(0).unwrap_or("").to_string(),
                schema_name:   r.get::<&str, _>(1).unwrap_or("").to_string(),
                table_name:    r.get::<&str, _>(2).unwrap_or("").to_string(),
                index_name:    r.get::<&str, _>(3).unwrap_or("").to_string(),
                user_seeks:    r.get::<i64, _>(4).unwrap_or(0) as u64,
                user_scans:    r.get::<i64, _>(5).unwrap_or(0) as u64,
                user_lookups:  r.get::<i64, _>(6).unwrap_or(0) as u64,
                user_updates:  r.get::<i64, _>(7).unwrap_or(0) as u64,
            });
        }
    }

    // Index metadata (key + included columns).
    //
    // We deliberately avoid STRING_AGG here: it was only introduced in SQL
    // Server 2017, and we advertise 2014+ support. The FOR XML PATH('') +
    // STUFF idiom is the portable string-aggregation pattern that works on
    // every version from 2014 onward. The `, TYPE).value('.', …)` step
    // round-trips through typed XML so column names containing XML-special
    // characters (`&`, `<`, `>`) come back un-escaped.
    let q_indexes = r#"
        SELECT s.name AS schema_name, t.name AS table_name, i.name AS index_name,
               i.is_unique, i.is_primary_key,
               STUFF((SELECT ',' + c.name
                      FROM sys.index_columns ic
                      JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
                      WHERE ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.is_included_column = 0
                      ORDER BY ic.key_ordinal
                      FOR XML PATH(''), TYPE).value('.', 'NVARCHAR(MAX)'), 1, 1, '') AS key_cols,
               STUFF((SELECT ',' + c.name
                      FROM sys.index_columns ic
                      JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
                      WHERE ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.is_included_column = 1
                      ORDER BY ic.key_ordinal
                      FOR XML PATH(''), TYPE).value('.', 'NVARCHAR(MAX)'), 1, 1, '') AS inc_cols
        FROM sys.indexes i
        JOIN sys.tables t ON t.object_id = i.object_id
        JOIN sys.schemas s ON s.schema_id = t.schema_id
        WHERE t.is_ms_shipped = 0 AND i.name IS NOT NULL
    "#;
    if let Ok(stream) = client.simple_query(q_indexes).await {
        let rows = stream.into_first_result().await.unwrap_or_default();
        for r in rows {
            let split = |s: Option<&str>| s.unwrap_or("").split(',').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect::<Vec<_>>();
            bundle.indexes.push(IndexMeta {
                schema_name:      r.get::<&str, _>(0).unwrap_or("").to_string(),
                table_name:       r.get::<&str, _>(1).unwrap_or("").to_string(),
                index_name:       r.get::<&str, _>(2).unwrap_or("").to_string(),
                is_unique:        r.get::<bool, _>(3).unwrap_or(false),
                is_primary_key:   r.get::<bool, _>(4).unwrap_or(false),
                key_columns:      split(r.get::<&str, _>(5)),
                included_columns: split(r.get::<&str, _>(6)),
            });
        }
    }

    // Missing indexes
    let q_missing = r#"
        SELECT s.name, t.name,
               ISNULL(mid.equality_columns, ''),
               ISNULL(mid.inequality_columns, ''),
               ISNULL(mid.included_columns, ''),
               migs.avg_user_impact,
               migs.user_seeks,
               migs.avg_total_user_cost
        FROM sys.dm_db_missing_index_groups mig
        JOIN sys.dm_db_missing_index_group_stats migs ON migs.group_handle = mig.index_group_handle
        JOIN sys.dm_db_missing_index_details mid ON mid.index_handle = mig.index_handle
        JOIN sys.objects t ON t.object_id = mid.object_id
        JOIN sys.schemas s ON s.schema_id = t.schema_id
        WHERE mid.database_id = DB_ID()
    "#;
    if let Ok(stream) = client.simple_query(q_missing).await {
        let rows = stream.into_first_result().await.unwrap_or_default();
        for r in rows {
            let strip = |s: &str| s.trim_matches(|c| c == '[' || c == ']').to_string();
            let split = |raw: &str| raw.split(',').map(|s| strip(s.trim())).filter(|s| !s.is_empty()).collect::<Vec<_>>();
            bundle.missing_indexes.push(MissingIndex {
                schema_name:        r.get::<&str, _>(0).unwrap_or("").to_string(),
                table_name:         r.get::<&str, _>(1).unwrap_or("").to_string(),
                equality_columns:   split(r.get::<&str, _>(2).unwrap_or("")),
                inequality_columns: split(r.get::<&str, _>(3).unwrap_or("")),
                included_columns:   split(r.get::<&str, _>(4).unwrap_or("")),
                avg_user_impact:    r.get::<f64, _>(5).unwrap_or(0.0),
                user_seeks:         r.get::<i64, _>(6).unwrap_or(0) as u64,
                avg_total_user_cost: r.get::<f64, _>(7).unwrap_or(0.0),
            });
        }
    }

    // Partition stats (size)
    let q_size = r#"
        SELECT s.name, t.name, i.name,
               SUM(p.rows) AS row_count,
               SUM(au.total_pages) * 8 AS reserved_kb,
               SUM(au.used_pages) * 8 AS used_kb,
               SUM(au.data_pages) * 8 AS data_kb
        FROM sys.tables t
        JOIN sys.schemas s ON s.schema_id = t.schema_id
        JOIN sys.indexes i ON i.object_id = t.object_id
        JOIN sys.partitions p ON p.object_id = t.object_id AND p.index_id = i.index_id
        JOIN sys.allocation_units au ON au.container_id = p.partition_id
        WHERE t.is_ms_shipped = 0
        GROUP BY s.name, t.name, i.name
    "#;
    if let Ok(stream) = client.simple_query(q_size).await {
        let rows = stream.into_first_result().await.unwrap_or_default();
        for r in rows {
            bundle.partition_stats.push(PartitionStats {
                schema_name: r.get::<&str, _>(0).unwrap_or("").to_string(),
                table_name:  r.get::<&str, _>(1).unwrap_or("").to_string(),
                index_name:  r.get::<&str, _>(2).map(|s| s.to_string()),
                row_count:   r.get::<i64, _>(3).unwrap_or(0) as u64,
                reserved_kb: r.get::<i64, _>(4).unwrap_or(0) as u64,
                used_kb:     r.get::<i64, _>(5).unwrap_or(0) as u64,
                data_kb:     r.get::<i64, _>(6).unwrap_or(0) as u64,
            });
        }
    }

    // Query-Store workload grounding: credit each user table we have index
    // metadata for with the busiest query observed against it, so the advisor
    // can rank "helps a query that runs N×/day". Best-effort — if Query Store
    // is OFF or the catalog views are unreadable we log and leave workload=[]
    // (the advisor degrades to the DMV's own seek counts). Never fails the
    // whole bundle.
    match query_store_workload(req, &bundle.indexes).await {
        Ok(workload) => bundle.workload = workload,
        Err(e) => tracing::warn!(
            target: "advisor",
            "Query Store workload grounding unavailable ({e}); ranking falls back to DMV seek counts"
        ),
    }

    Ok(bundle)
}

// ===========================================================================
// Live Pulse — instantaneous server vitals for the real-time WATCH
// view. Every query hits ONLY DMVs (sys.dm_*); we never read user table rows.
// Cumulative counters (batch req, IO bytes) are returned raw + a server clock
// so the UI can compute per-second rates between successive polls, exactly the
// the way any live counter does. Each query is best-effort: a login missing
// VIEW SERVER STATE still gets whatever it's allowed to see.
// ===========================================================================

/// Benign idle/background wait types that are always "waiting" on a healthy
/// server (the well-known community ignore-list). Excluded from the live
/// waiting-tasks count and the resource-waits panel so an idle instance does
/// not read as "28 tasks waiting". Kept as a quoted, comma-separated SQL list.
const BENIGN_WAITS: &str = "\
'SLEEP_TASK','LAZYWRITER_SLEEP','LOGMGR_QUEUE','CHECKPOINT_QUEUE',\
'REQUEST_FOR_DEADLOCK_SEARCH','XE_TIMER_EVENT','XE_DISPATCHER_WAIT','XE_DISPATCHER_JOIN',\
'BROKER_TO_FLUSH','BROKER_TASK_STOP','BROKER_EVENTHANDLER','BROKER_RECEIVE_WAITFOR',\
'SQLTRACE_BUFFER_FLUSH','SQLTRACE_INCREMENTAL_FLUSH_SLEEP','SQLTRACE_WAIT_ENTRIES',\
'CLR_AUTO_EVENT','CLR_MANUAL_EVENT','DISPATCHER_QUEUE_SEMAPHORE','FT_IFTS_SCHEDULER_IDLE_WAIT',\
'WAITFOR','DBMIRROR_DBM_EVENT','DBMIRROR_EVENTS_QUEUE','DBMIRROR_WORKER_QUEUE',\
'HADR_FILESTREAM_IOMGR_IOCOMPLETION','HADR_WORK_QUEUE','HADR_TIMER_TASK','HADR_CLUSAPI_CALL',\
'KSOURCE_WAKEUP','LOGMGR_FLUSH','ONDEMAND_TASK_QUEUE','PWAIT_ALL_COMPONENTS_INITIALIZED',\
'QDS_PERSIST_TASK_MAIN_LOOP_SLEEP','QDS_ASYNC_QUEUE','QDS_CLEANUP_STALE_QUERIES_TASK_MAIN_LOOP_SLEEP',\
'QDS_SHUTDOWN_QUEUE','SP_SERVER_DIAGNOSTICS_SLEEP','SLEEP_DBSTARTUP','SLEEP_DCOMSTARTUP',\
'SLEEP_MASTERDBREADY','SLEEP_MASTERMDREADY','SLEEP_MASTERUPGRADED','SLEEP_SYSTEMTASK',\
'WAIT_XTP_HOST_WAIT','WAIT_XTP_OFFLINE_CKPT_NEW_LOG','WAIT_XTP_CKPT_CLOSE','WAIT_FOR_RESULTS',\
'DIRTY_PAGE_POLL','POPULATE_LOCK_ORDINALS','PREEMPTIVE_OS_FLUSHFILEBUFFERS','PREEMPTIVE_XE_GETTARGETSTATE',\
'BROKER_TRANSMITTER','PVS_PREALLOCATE','HADR_NOTIFICATION_DEQUEUE','HADR_NOTIFICATION_WORKER_EXCLUSIVE_ACCESS',\
'VDI_CLIENT_OTHER','PARALLEL_REDO_DRAIN_WORKER','PARALLEL_REDO_LOG_CACHE','PARALLEL_REDO_TRAN_LIST',\
'PARALLEL_REDO_WORKER_WAIT_WORK','PARALLEL_REDO_WORKER_SYNC','UCS_SESSION_REGISTRATION','VDI_CLIENT_COMPLETED',\
'SOS_WORK_DISPATCHER','STARTUP_DEPENDENCY_MANAGER','SLEEP_TEMPDBSTARTUP','BACKUPTHREAD'";

#[derive(Debug, Clone, serde::Serialize)]
pub struct LiveWait {
    pub wait_type: String,
    pub tasks: i64,
    pub wait_ms: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LiveSession {
    pub session_id: i64,
    pub status: String,
    pub command: String,
    pub duration_ms: i64,
    pub cpu_ms: i64,
    pub logical_reads: i64,
    pub blocked_by: i64,
    pub wait_type: Option<String>,
    pub database: String,
    pub login: String,
    pub host: String,
    pub program: String,
    pub sql_preview: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LiveMetrics {
    pub server_time_ms: i64,
    pub cpu_sql_pct: Option<i64>,
    pub cpu_other_pct: Option<i64>,
    pub waiting_tasks: i64,
    pub active_requests: i64,
    pub blocked_requests: i64,
    pub user_sessions: i64,
    // cumulative perf counters (UI derives /sec rates)
    pub batch_requests_total: i64,
    pub compilations_total: i64,
    pub recompilations_total: i64,
    pub transactions_total: i64,
    pub page_life_expectancy: Option<i64>,
    // cumulative IO (UI derives MB/sec)
    pub io_read_bytes_total: i64,
    pub io_write_bytes_total: i64,
    pub io_stall_read_ms: i64,
    pub io_stall_write_ms: i64,
    pub top_waits: Vec<LiveWait>,
    pub sessions: Vec<LiveSession>,

    // ---- deep vitals (the community real-time script-style real-time pressure) -------------
    // CPU PRESSURE: workers ready-to-run but waiting on a CPU, summed over the
    // schedulers that run user work, plus the scheduler count for context.
    pub online_schedulers: Option<i64>,
    pub runnable_tasks: Option<i64>,
    pub scheduler_work_queue: Option<i64>,
    // MEMORY HEADROOM: pending workspace-memory grants + buffer-pool sizing.
    // (page_life_expectancy already exists above and is the PLE side of this.)
    pub pending_memory_grants: Option<i64>,
    pub target_server_memory_kb: Option<i64>,
    pub total_server_memory_kb: Option<i64>,
    // IO LATENCY: instance-wide avg ms-per-read / ms-per-write right now. These
    // are lifetime cumulative ratios (stall/op); the sentinel time-series holds
    // the per-window deltas. Useful as a single "is storage slow" gauge.
    pub avg_read_latency_ms: Option<f64>,
    pub avg_write_latency_ms: Option<f64>,
    // TEMPDB CONTENTION: live PAGELATCH waiters on tempdb PFS/GAM/SGAM pages.
    pub tempdb_pagelatch_waiters: Option<i64>,
    pub tempdb_data_files: Option<i64>,
    // PLAN CACHE: single-use ad-hoc plan count/size vs the whole cache.
    pub single_use_plan_count: Option<i64>,
    pub single_use_plan_kb: Option<i64>,
    pub total_plan_count: Option<i64>,
}

/// One real-time snapshot of server vitals. Polled on an interval by the UI.
pub async fn pull_live_metrics(req: &ConnectReq) -> anyhow::Result<LiveMetrics> {
    let mut client = open(req).await?;
    let mut m = LiveMetrics::default();

    // --- server clock (ms since epoch) so the UI can compute rate = Δcounter/Δt
    if let Ok(s) = client
        .simple_query("SELECT DATEDIFF_BIG(MILLISECOND, '19700101', SYSUTCDATETIME())")
        .await
    {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.server_time_ms = r.get::<i64, _>(0).unwrap_or(0);
            }
        }
    }

    // --- recent CPU split (SQL process vs everything else) from the scheduler
    //     monitor ring buffer. Best-effort: XML shredding can vary by edition.
    let cpu_sql = r#"
        SELECT TOP 1
            record.value('(./Record/SchedulerMonitorEvent/SystemHealth/ProcessUtilization)[1]','int') AS sql_cpu,
            record.value('(./Record/SchedulerMonitorEvent/SystemHealth/SystemIdle)[1]','int') AS idle
        FROM (
            SELECT CONVERT(xml, record) AS record, timestamp
            FROM sys.dm_os_ring_buffers
            WHERE ring_buffer_type = N'RING_BUFFER_SCHEDULER_MONITOR'
              AND record LIKE '%<SystemHealth>%'
        ) AS x
        ORDER BY x.timestamp DESC;
    "#;
    if let Ok(s) = client.simple_query(cpu_sql).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                let sql_cpu = r.get::<i32, _>(0).unwrap_or(0) as i64;
                let idle = r.get::<i32, _>(1).unwrap_or(0) as i64;
                m.cpu_sql_pct = Some(sql_cpu.clamp(0, 100));
                m.cpu_other_pct = Some((100 - idle - sql_cpu).clamp(0, 100));
            }
        }
    }

    // --- instantaneous counts (all CAST to BIGINT so they read as one i64 row).
    //     Waiting-tasks excludes benign idle/background waits so the number
    //     reflects REAL contention, not housekeeping — avoiding idle false alarms.
    let counts = r#"
        SELECT
          CAST((SELECT COUNT(*) FROM sys.dm_os_waiting_tasks
                WHERE session_id IS NOT NULL AND wait_type NOT IN (BENIGN_WAITS)) AS BIGINT),
          CAST((SELECT COUNT(*) FROM sys.dm_exec_requests r
                JOIN sys.dm_exec_sessions s ON s.session_id = r.session_id
                WHERE s.is_user_process = 1 AND r.session_id <> @@SPID) AS BIGINT),
          CAST((SELECT COUNT(*) FROM sys.dm_exec_requests WHERE blocking_session_id <> 0) AS BIGINT),
          CAST((SELECT COUNT(*) FROM sys.dm_exec_sessions WHERE is_user_process = 1) AS BIGINT);
    "#.replace("BENIGN_WAITS", BENIGN_WAITS);
    if let Ok(s) = client.simple_query(&counts).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.waiting_tasks = r.get::<i64, _>(0).unwrap_or(0);
                m.active_requests = r.get::<i64, _>(1).unwrap_or(0);
                m.blocked_requests = r.get::<i64, _>(2).unwrap_or(0);
                m.user_sessions = r.get::<i64, _>(3).unwrap_or(0);
            }
        }
    }

    // --- cumulative perf counters (UI turns these into /sec rates)
    let perf = r#"
        SELECT
          MAX(CASE WHEN RTRIM(counter_name)='Batch Requests/sec' THEN cntr_value END),
          MAX(CASE WHEN RTRIM(counter_name)='SQL Compilations/sec' THEN cntr_value END),
          MAX(CASE WHEN RTRIM(counter_name)='SQL Re-Compilations/sec' THEN cntr_value END),
          MAX(CASE WHEN RTRIM(counter_name)='Transactions/sec' AND RTRIM(instance_name)='_Total' THEN cntr_value END),
          MAX(CASE WHEN RTRIM(counter_name)='Page life expectancy' THEN cntr_value END)
        FROM sys.dm_os_performance_counters
        WHERE RTRIM(counter_name) IN
          ('Batch Requests/sec','SQL Compilations/sec','SQL Re-Compilations/sec','Transactions/sec','Page life expectancy');
    "#;
    if let Ok(s) = client.simple_query(perf).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.batch_requests_total = r.get::<i64, _>(0).unwrap_or(0);
                m.compilations_total = r.get::<i64, _>(1).unwrap_or(0);
                m.recompilations_total = r.get::<i64, _>(2).unwrap_or(0);
                m.transactions_total = r.get::<i64, _>(3).unwrap_or(0);
                let ple = r.get::<i64, _>(4).unwrap_or(-1);
                if ple >= 0 { m.page_life_expectancy = Some(ple); }
            }
        }
    }

    // --- cumulative IO across all data/log files (UI turns into MB/sec)
    let io = r#"
        SELECT
          CAST(SUM(num_of_bytes_read) AS BIGINT),
          CAST(SUM(num_of_bytes_written) AS BIGINT),
          CAST(SUM(io_stall_read_ms) AS BIGINT),
          CAST(SUM(io_stall_write_ms) AS BIGINT)
        FROM sys.dm_io_virtual_file_stats(NULL, NULL);
    "#;
    if let Ok(s) = client.simple_query(io).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.io_read_bytes_total = r.get::<i64, _>(0).unwrap_or(0);
                m.io_write_bytes_total = r.get::<i64, _>(1).unwrap_or(0);
                m.io_stall_read_ms = r.get::<i64, _>(2).unwrap_or(0);
                m.io_stall_write_ms = r.get::<i64, _>(3).unwrap_or(0);
            }
        }
    }

    // --- top resource waits happening right now
    let waits = r#"
        SELECT TOP (8) wait_type,
               CAST(COUNT(*) AS BIGINT) AS tasks,
               CAST(SUM(wait_duration_ms) AS BIGINT) AS wait_ms
        FROM sys.dm_os_waiting_tasks
        WHERE wait_type IS NOT NULL AND session_id IS NOT NULL
          AND wait_type NOT IN (BENIGN_WAITS)
        GROUP BY wait_type
        ORDER BY tasks DESC, wait_ms DESC;
    "#.replace("BENIGN_WAITS", BENIGN_WAITS);
    if let Ok(s) = client.simple_query(&waits).await {
        if let Ok(rows) = s.into_first_result().await {
            for r in rows {
                m.top_waits.push(LiveWait {
                    wait_type: r.get::<&str, _>(0).unwrap_or("").to_string(),
                    tasks: r.get::<i64, _>(1).unwrap_or(0),
                    wait_ms: r.get::<i64, _>(2).unwrap_or(0),
                });
            }
        }
    }

    // --- live user requests ("running now"). sql_preview MUST be cast to
    //     a bounded NVARCHAR or tiberius returns NULL for the nvarchar(max) text.
    let sessions = r#"
        SELECT TOP (50)
            r.session_id,
            r.status,
            r.command,
            DATEDIFF(MILLISECOND, r.start_time, SYSUTCDATETIME()) AS duration_ms,
            r.cpu_time,
            r.logical_reads,
            r.blocking_session_id,
            r.wait_type,
            DB_NAME(r.database_id) AS db,
            s.login_name,
            ISNULL(s.host_name,'') AS host_name,
            ISNULL(s.program_name,'') AS program,
            CAST(LEFT(REPLACE(REPLACE(t.text, CHAR(13), ' '), CHAR(10), ' '), 300) AS NVARCHAR(300)) AS sql_preview
        FROM sys.dm_exec_requests AS r
        JOIN sys.dm_exec_sessions AS s ON s.session_id = r.session_id
        OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) AS t
        WHERE r.session_id <> @@SPID AND s.is_user_process = 1
        ORDER BY r.cpu_time DESC;
    "#;
    if let Ok(s) = client.simple_query(sessions).await {
        if let Ok(rows) = s.into_first_result().await {
            for r in rows {
                let blocked_raw = r.get::<i16, _>(6).unwrap_or(0);
                m.sessions.push(LiveSession {
                    session_id: r.get::<i16, _>(0).unwrap_or(0) as i64,
                    status: r.get::<&str, _>(1).unwrap_or("").to_string(),
                    command: r.get::<&str, _>(2).unwrap_or("").to_string(),
                    duration_ms: r.get::<i32, _>(3).unwrap_or(0) as i64,
                    cpu_ms: r.get::<i32, _>(4).unwrap_or(0) as i64,
                    logical_reads: r.get::<i64, _>(5).unwrap_or(0),
                    blocked_by: if blocked_raw > 0 { blocked_raw as i64 } else { 0 },
                    wait_type: r.get::<&str, _>(7).map(|s| s.to_string()),
                    database: r.get::<&str, _>(8).unwrap_or("").to_string(),
                    login: r.get::<&str, _>(9).unwrap_or("").to_string(),
                    host: r.get::<&str, _>(10).unwrap_or("").to_string(),
                    program: r.get::<&str, _>(11).unwrap_or("").to_string(),
                    sql_preview: r.get::<&str, _>(12).unwrap_or("").to_string(),
                });
            }
        }
    }

    // --- CPU PRESSURE: runnable tasks + work queue over VISIBLE ONLINE
    //     schedulers. Sustained runnable_tasks > 0 means workers are queued for
    //     CPU. All CAST to BIGINT (one i64 row). Best-effort.
    let cpu_pressure = r#"
        SELECT
            CAST(COUNT(*)                   AS BIGINT),
            CAST(SUM(runnable_tasks_count)  AS BIGINT),
            CAST(SUM(work_queue_count)      AS BIGINT)
        FROM sys.dm_os_schedulers
        WHERE status = 'VISIBLE ONLINE';
    "#;
    if let Ok(s) = client.simple_query(cpu_pressure).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.online_schedulers = Some(r.get::<i64, _>(0).unwrap_or(0));
                m.runnable_tasks = Some(r.get::<i64, _>(1).unwrap_or(0));
                m.scheduler_work_queue = Some(r.get::<i64, _>(2).unwrap_or(0));
            }
        }
    }

    // --- MEMORY HEADROOM: pending workspace-memory grants + target/total
    //     server memory (KB). PLE is already captured in the perf block above.
    let mem_grants = r#"
        SELECT
            CAST(ISNULL(SUM(waiter_count), 0) AS BIGINT)
        FROM sys.dm_exec_query_resource_semaphores;
    "#;
    if let Ok(s) = client.simple_query(mem_grants).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.pending_memory_grants = Some(r.get::<i64, _>(0).unwrap_or(0));
            }
        }
    }
    let mem_totals = r#"
        SELECT
            CAST(MAX(CASE WHEN RTRIM(counter_name) = 'Target Server Memory (KB)'
                          THEN cntr_value END) AS BIGINT),
            CAST(MAX(CASE WHEN RTRIM(counter_name) = 'Total Server Memory (KB)'
                          THEN cntr_value END) AS BIGINT)
        FROM sys.dm_os_performance_counters
        WHERE RTRIM(counter_name) IN ('Target Server Memory (KB)','Total Server Memory (KB)');
    "#;
    if let Ok(s) = client.simple_query(mem_totals).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.target_server_memory_kb = r.get::<i64, _>(0);
                m.total_server_memory_kb = r.get::<i64, _>(1);
            }
        }
    }

    // --- IO LATENCY: instance-wide avg ms per read / write (lifetime ratio).
    //     CAST to FLOAT so the division returns a real, not integer-truncated.
    let io_latency = r#"
        SELECT
            CAST(CASE WHEN SUM(num_of_reads)  > 0
                 THEN SUM(io_stall_read_ms)  * 1.0 / SUM(num_of_reads)  ELSE 0 END AS FLOAT),
            CAST(CASE WHEN SUM(num_of_writes) > 0
                 THEN SUM(io_stall_write_ms) * 1.0 / SUM(num_of_writes) ELSE 0 END AS FLOAT)
        FROM sys.dm_io_virtual_file_stats(NULL, NULL);
    "#;
    if let Ok(s) = client.simple_query(io_latency).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.avg_read_latency_ms = r.get::<f64, _>(0);
                m.avg_write_latency_ms = r.get::<f64, _>(1);
            }
        }
    }

    // --- TEMPDB CONTENTION: live PAGELATCH waiters on tempdb (db_id 2)
    //     allocation pages, plus the tempdb data-file count for context.
    let tempdb = r#"
        SELECT
            CAST((SELECT COUNT(*) FROM sys.dm_os_waiting_tasks
                  WHERE wait_type LIKE 'PAGELATCH%'
                    AND resource_description LIKE '2:%') AS BIGINT),
            CAST((SELECT COUNT(*) FROM sys.master_files
                  WHERE database_id = 2 AND type_desc = 'ROWS') AS BIGINT);
    "#;
    if let Ok(s) = client.simple_query(tempdb).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.tempdb_pagelatch_waiters = Some(r.get::<i64, _>(0).unwrap_or(0));
                m.tempdb_data_files = Some(r.get::<i64, _>(1).unwrap_or(0));
            }
        }
    }

    // --- PLAN CACHE: single-use ad-hoc plans (count + KB) vs total plan count.
    let plan_cache = r#"
        SELECT
            CAST(SUM(CASE WHEN objtype = 'Adhoc' AND usecounts = 1 THEN 1 ELSE 0 END) AS BIGINT),
            CAST(SUM(CASE WHEN objtype = 'Adhoc' AND usecounts = 1 THEN size_in_bytes ELSE 0 END) / 1024 AS BIGINT),
            CAST(COUNT(*) AS BIGINT)
        FROM sys.dm_exec_cached_plans;
    "#;
    if let Ok(s) = client.simple_query(plan_cache).await {
        if let Ok(rows) = s.into_first_result().await {
            if let Some(r) = rows.into_iter().next() {
                m.single_use_plan_count = Some(r.get::<i64, _>(0).unwrap_or(0));
                m.single_use_plan_kb = Some(r.get::<i64, _>(1).unwrap_or(0));
                m.total_plan_count = Some(r.get::<i64, _>(2).unwrap_or(0));
            }
        }
    }

    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::references_table_token;

    #[test]
    fn word_boundary_does_not_match_longer_token() {
        // The core requirement: "Order" must NOT match the longer table "Orders".
        assert!(!references_table_token("SELECT * FROM dbo.Orders", "Order"));
        assert!(!references_table_token("SELECT * FROM Ordered", "Order"));
        assert!(!references_table_token("SELECT * FROM PreOrder", "Order"));
        assert!(!references_table_token("SELECT * FROM Order_Items", "Order"));
    }

    #[test]
    fn matches_bare_bracketed_and_schema_qualified() {
        assert!(references_table_token("SELECT * FROM Orders WHERE x=1", "Orders"));
        assert!(references_table_token("SELECT * FROM [Orders]", "Orders"));
        assert!(references_table_token("SELECT * FROM dbo.Orders o", "Orders"));
        // Case-insensitive.
        assert!(references_table_token("select * from ORDERS", "Orders"));
        assert!(references_table_token("select * from orders", "Orders"));
    }

    #[test]
    fn matches_at_string_boundaries_and_with_punctuation() {
        // Token at the very start / end of the text.
        assert!(references_table_token("Orders", "Orders"));
        assert!(references_table_token("JOIN Orders", "Orders"));
        // Trailing punctuation (comma, semicolon, paren) is a word boundary.
        assert!(references_table_token("FROM Orders, Customers", "Orders"));
        assert!(references_table_token("FROM Orders;", "Orders"));
        assert!(references_table_token("COUNT(Orders)", "Orders"));
    }

    #[test]
    fn empty_ident_or_no_match_is_false() {
        assert!(!references_table_token("SELECT 1", ""));
        assert!(!references_table_token("SELECT * FROM Customers", "Orders"));
        assert!(!references_table_token("", "Orders"));
    }
}
