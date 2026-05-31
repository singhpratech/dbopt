//! Opens a tiberius client against a `ConnectionInfo`. The backend has its own
//! copy of this pattern; we duplicate it here to avoid a cross-crate dep.

use crate::ConnectionInfo;
use std::time::Duration;
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

pub type SqlClient = Client<tokio_util::compat::Compat<TcpStream>>;

/// Bound the initial TCP connect so an unreachable instance fails a poll tick
/// fast instead of pinning the scheduler task on the OS default (60–120s).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Map a tiberius connect/login failure to a safe category message. The config
/// carries the password, so we never propagate the raw error text.
fn safe_connect_err(e: &tiberius::error::Error) -> anyhow::Error {
    let raw = e.to_string().to_ascii_lowercase();
    let msg = if raw.contains("login failed")
        || raw.contains("password")
        || raw.contains("authentication")
        || raw.contains("user")
    {
        "authentication failed — check the SQL login username and password"
    } else if raw.contains("cannot open database") || raw.contains("database") {
        "could not open the requested database — check the name and login access"
    } else if raw.contains("certificate") || raw.contains("tls") || raw.contains("ssl") {
        "TLS/certificate error — set trust_cert or install a trusted certificate"
    } else {
        "could not connect to SQL Server — check the server address and port"
    };
    anyhow::anyhow!(msg)
}

pub async fn open(info: &ConnectionInfo) -> anyhow::Result<SqlClient> {
    let mut config = Config::new();
    let parts: Vec<&str> = info.server.splitn(2, ',').collect();
    config.host(parts[0]);
    if let Some(port) = parts.get(1).and_then(|p| p.parse::<u16>().ok()) {
        config.port(port);
    } else {
        config.port(1433);
    }
    if let Some(db) = info.database.as_deref() {
        if !db.is_empty() { config.database(db); }
    }
    match (info.user.as_deref(), info.password.as_deref()) {
        (Some(u), Some(p)) if !u.is_empty() => { config.authentication(AuthMethod::sql_server(u, p)); }
        _ => {
            // No SQL credentials supplied → fall back to integrated (current
            // Windows user) auth, mirroring the backend. On the Windows build
            // this is native SSPI (winauth); on Linux it needs the Kerberos/
            // GSSAPI `integrated-auth` feature, so a default Linux build returns
            // an actionable hint instead of a confusing connection error.
            #[cfg(windows)]
            { config.authentication(AuthMethod::Integrated); }
            #[cfg(all(unix, feature = "integrated-auth"))]
            { config.authentication(AuthMethod::Integrated); }
            #[cfg(not(any(windows, all(unix, feature = "integrated-auth"))))]
            {
                anyhow::bail!(
                    "sentinel needs SQL authentication (user + password). Windows integrated auth is \
available on the Windows build; on Linux, rebuild sentinel with `--features integrated-auth` (GSSAPI/Kerberos)."
                );
            }
        }
    }
    if info.trust_cert.unwrap_or(false) {
        config.trust_cert();
    }
    // Tag every sentinel connection so its own polling queries can be filtered
    // out of the activity feed (see poll::live / poll::query_store).
    config.application_name("dbopt-sentinel");
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(config.get_addr()))
        .await
        .map_err(|_| anyhow::anyhow!("connection to SQL Server timed out after {}s", CONNECT_TIMEOUT.as_secs()))?
        .map_err(|_| anyhow::anyhow!("could not reach SQL Server — check the server address and port"))?;
    tcp.set_nodelay(true)?;
    let client = Client::connect(config, tcp.compat_write())
        .await
        .map_err(|e| safe_connect_err(&e))?;
    Ok(client)
}
