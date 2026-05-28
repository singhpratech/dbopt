//! Opens a tiberius client against a `ConnectionInfo`. The backend has its own
//! copy of this pattern; we duplicate it here to avoid a cross-crate dep.

use crate::ConnectionInfo;
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

pub type SqlClient = Client<tokio_util::compat::Compat<TcpStream>>;

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
        (Some(u), Some(p)) if !u.is_empty() => config.authentication(AuthMethod::sql_server(u, p)),
        _ => {
            anyhow::bail!(
                "sentinel requires SQL authentication (user + password). Integrated auth is not yet wired in this crate."
            );
        }
    }
    if info.trust_cert.unwrap_or(false) {
        config.trust_cert();
    }
    // Tag every sentinel connection so its own polling queries can be filtered
    // out of the activity feed (see poll::live / poll::query_store).
    config.application_name("sqlopt-sentinel");
    let tcp = TcpStream::connect(config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    let client = Client::connect(config, tcp.compat_write()).await?;
    Ok(client)
}
