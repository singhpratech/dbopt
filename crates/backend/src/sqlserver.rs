use crate::routes::ConnectReq;
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
    match (req.user.as_deref(), req.password.as_deref()) {
        (Some(u), Some(p)) if !u.is_empty() => config.authentication(AuthMethod::sql_server(u, p)),
        _ => {
            #[cfg(feature = "integrated-auth")]
            { config.authentication(AuthMethod::Integrated); }
            #[cfg(not(feature = "integrated-auth"))]
            {
                return Err(anyhow::anyhow!(
                    "Integrated (Windows) auth requires the backend to be built with `--features integrated-auth` (uses GSSAPI/Kerberos on Linux). \
Either rebuild with that feature, or switch to SQL authentication and provide a user + password."
                ));
            }
        }
    }
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
pub async fn list_databases(req: &ConnectReq) -> anyhow::Result<Vec<String>> {
    let mut client = open(req).await?;
    let stream = client
        .simple_query(
            "SELECT name FROM sys.databases WHERE database_id > 4 AND state_desc = 'ONLINE' ORDER BY name",
        )
        .await?;
    let rows = stream.into_first_result().await?;
    let mut names = Vec::with_capacity(rows.len());
    for r in rows {
        if let Some(n) = r.get::<&str, _>(0) {
            names.push(n.to_string());
        }
    }
    Ok(names)
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

    Ok(m)
}
