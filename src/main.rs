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
    tracing::info!(address = %listener.local_addr()?, stage = "M1", "service ready");
    server
        .serve(listener, async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
        })
        .await
}
