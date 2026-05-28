use crate::routes::ConnectReq;
use analyzer_core::dmv::{DmvBundle, IndexUsage, IndexMeta, MissingIndex, PartitionStats};
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

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
    let tcp = TcpStream::connect(config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    let client = Client::connect(config, tcp.compat_write()).await?;
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

    // Index metadata (key + included columns)
    let q_indexes = r#"
        SELECT s.name AS schema_name, t.name AS table_name, i.name AS index_name,
               i.is_unique, i.is_primary_key,
               STRING_AGG(CASE WHEN ic.is_included_column = 0 THEN c.name END, ',') WITHIN GROUP (ORDER BY ic.key_ordinal) AS key_cols,
               STRING_AGG(CASE WHEN ic.is_included_column = 1 THEN c.name END, ',') AS inc_cols
        FROM sys.indexes i
        JOIN sys.tables t ON t.object_id = i.object_id
        JOIN sys.schemas s ON s.schema_id = t.schema_id
        JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
        JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
        WHERE t.is_ms_shipped = 0 AND i.name IS NOT NULL
        GROUP BY s.name, t.name, i.name, i.is_unique, i.is_primary_key
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
