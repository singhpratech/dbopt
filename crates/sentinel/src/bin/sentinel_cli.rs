//! `dbopt-sentinel` — CLI front-end for the sentinel daemon.
//!
//! Three modes:
//!   poll-once   run every poller exactly once against an instance
//!   run         start the long-running daemon (blocks until ^C)
//!   report      render a markdown report from whatever data is already
//!               in `~/.dbopt/sentinel.db`
//!
//! All connection info comes from environment variables so the binary
//! can be wrapped in a systemd unit / launchd plist / Windows service
//! without surfacing secrets on the command line:
//!
//!   DBOPT_SERVER       host[,port]      (default localhost,1433)
//!   DBOPT_DB           database name    (optional)
//!   DBOPT_USER         SQL login
//!   DBOPT_PASSWORD     SQL password
//!   DBOPT_TRUST_CERT   "1" to skip TLS validation
//!   DBOPT_DATA_DIR     where to put sentinel.db (default ~/.dbopt)
//!   DBOPT_INSTANCE     display name for the instance (default = server)

use std::time::Duration;

use sentinel::{
    poll, report,
    storage::{Storage, TimeRange},
    ConnectionInfo, InstanceConfig, Sentinel, SentinelConfig,
};

fn install_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sentinel=info".into()),
        )
        .init();
}

fn env_conn() -> anyhow::Result<ConnectionInfo> {
    Ok(ConnectionInfo {
        server: std::env::var("DBOPT_SERVER").unwrap_or_else(|_| "localhost,1433".into()),
        database: std::env::var("DBOPT_DB").ok().filter(|s| !s.is_empty()),
        user: std::env::var("DBOPT_USER").ok().filter(|s| !s.is_empty()),
        password: std::env::var("DBOPT_PASSWORD").ok().filter(|s| !s.is_empty()),
        trust_cert: Some(std::env::var("DBOPT_TRUST_CERT").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(true)),
    })
}

fn usage() -> ! {
    eprintln!("usage: dbopt-sentinel <poll-once | run | report> [days]");
    std::process::exit(2);
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    install_tracing();

    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "poll-once".into());
    let db_path = SentinelConfig::default_db_path();
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let storage = std::sync::Arc::new(Storage::open(&db_path)?);

    match mode.as_str() {
        "poll-once" => {
            let conn = env_conn()?;
            tracing::info!("poll-once against {} (db={})", conn.server, db_path.display());
            // Fail fast at startup if creds are missing — better than waiting
            // for each poller to print the same error.
            if conn.user.as_deref().unwrap_or("").is_empty() {
                anyhow::bail!("DBOPT_USER + DBOPT_PASSWORD are required for sentinel poll-once");
            }
            run_once(&conn, &storage).await;
            tracing::info!("poll-once complete · db at {}", db_path.display());
        }
        "run" => {
            let instance_name = std::env::var("DBOPT_INSTANCE")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    std::env::var("DBOPT_SERVER").unwrap_or_else(|_| "localhost".into())
                });
            let cfg = SentinelConfig {
                instances: vec![InstanceConfig {
                    name: instance_name,
                    conn: env_conn()?,
                    cadences: Default::default(),
                    enabled: true,
                }],
                db_path: db_path.clone(),
                retention_days: sentinel::default_retention_days(),
                alerting: Default::default(),
                alert_eval_secs: sentinel::default_vitals_secs(),
            };
            let sentinel = Sentinel::start(cfg).await?;
            tracing::info!("sentinel running · ^C to stop · db {}", db_path.display());
            tokio::signal::ctrl_c().await.ok();
            sentinel.stop().await;
        }
        "report" => {
            let days: i64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(7);
            let report = report::render_weekly(&storage, TimeRange::last_days(days));
            println!("{}", report::render_markdown(&report));
        }
        _ => usage(),
    }
    Ok(())
}

/// Sequentially calls every poller once. We don't parallelize here because
/// the SQLite writer is serialized and ~seconds total is plenty fast.
async fn run_once(conn: &ConnectionInfo, storage: &Storage) {
    run_one("query_store", poll::query_store::poll_query_store(conn, storage)).await;
    run_one("live",        poll::live::poll_live_requests(conn, storage)).await;
    run_one("waits",       poll::waits::poll_wait_stats(conn, storage)).await;
    run_one("deadlocks",   poll::deadlocks::poll_deadlocks(conn, storage)).await;
    run_one("index_usage", poll::index_usage::poll_index_usage_delta(conn, storage)).await;
    run_one("sizes",       poll::sizes::poll_sizes(conn, storage)).await;
}

async fn run_one(name: &str, fut: impl std::future::Future<Output = anyhow::Result<()>>) {
    let started = std::time::Instant::now();
    match tokio::time::timeout(Duration::from_secs(60), fut).await {
        Ok(Ok(())) => tracing::info!(target: "sentinel::cli", "poller {name} ok in {:?}", started.elapsed()),
        Ok(Err(e)) => tracing::warn!(target: "sentinel::cli", "poller {name} failed: {e:#}"),
        Err(_)     => tracing::warn!(target: "sentinel::cli", "poller {name} timed out after 60s"),
    }
}
