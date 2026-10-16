use syncserver::{Config, Result, Server};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    if let Err(error) = dispatch().await {
        // Error Display is sanitized: no credential text, file paths or SQL.
        eprintln!("syncserver: {error}");
        std::process::exit(1);
    }
}

/// Command dispatch. Configuration is parsed before the logging pipeline starts, so the
/// pipeline follows the configured levels; anything emitted earlier still reaches stderr.
async fn dispatch() -> Result<()> {
    let config = Config::from_env()?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "users") {
        return syncserver::user_cli::run(config, &args[1..]);
    }
    if args.first().is_some_and(|arg| arg == "logs") {
        return syncserver::logging::run(config, &args[1..]);
    }
    syncserver::logging::install(config.logging())?;
    let route = route(config, &args).await;
    // The log writer is closed last so shutdown records from every other subsystem land.
    route.and(syncserver::logging::shutdown())
}

async fn route(config: Config, args: &[String]) -> Result<()> {
    if !args.is_empty() {
        return Err(syncserver::Error::Config(
            "usage: syncserver [users ... | logs ...]",
        ));
    }
    let address = config.bind_address();
    let log = config.logging().clone();
    let server = Server::open(config).await?;
    let listener = match TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            server.shutdown().await?;
            return Err(error.into());
        }
    };
    let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
    let mut terminate = match terminate {
        Ok(signal) => signal,
        Err(error) => {
            server.shutdown().await?;
            return Err(error.into());
        }
    };
    tracing::info!(
        address = %listener.local_addr()?,
        stage = "M4",
        log_level = log.level.as_str(),
        log_persist_level = log.persist_level.as_str(),
        log_payload_detail = log.payload_detail,
        log_retention_raw_secs = log.retention_raw_secs,
        log_archive_max_bytes = log.archive_max_bytes,
        "service ready"
    );
    server
        .serve(listener, async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
        })
        .await
}
