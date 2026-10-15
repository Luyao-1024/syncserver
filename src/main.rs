use syncserver::{Config, Result, Server};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    if let Err(error) = run().await {
        // Error Display is sanitized: no credential text, file paths or SQL.
        eprintln!("syncserver: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let config = Config::from_env()?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "users") {
        return syncserver::user_cli::run(config, &args[1..]);
    }
    if !args.is_empty() {
        return Err(syncserver::Error::Config("usage: syncserver [users ...]"));
    }
    let address = config.bind_address();
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
    tracing::info!(address = %listener.local_addr()?, stage = "M4", "service ready");
    server
        .serve(listener, async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
        })
        .await
}
