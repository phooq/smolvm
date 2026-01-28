//! HTTP API server command.

use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;

use smolvm::api::state::ApiState;
use smolvm::Result;

/// Start the HTTP API server for programmatic control.
#[derive(Parser, Debug)]
#[command(about = "Start the HTTP API server for programmatic sandbox management")]
#[command(after_long_help = "\
Sandboxes persist independently of the server - they continue running even if the server stops.

API ENDPOINTS:
  GET    /health                       Health check
  POST   /api/v1/sandboxes             Create sandbox
  GET    /api/v1/sandboxes             List sandboxes
  GET    /api/v1/sandboxes/:id         Get sandbox status
  POST   /api/v1/sandboxes/:id/start   Start sandbox
  POST   /api/v1/sandboxes/:id/stop    Stop sandbox
  POST   /api/v1/sandboxes/:id/exec    Execute command
  DELETE /api/v1/sandboxes/:id         Delete sandbox

EXAMPLES:
  smolvm serve                         Listen on 127.0.0.1:8080 (default)
  smolvm serve -l 0.0.0.0:9000         Listen on all interfaces, port 9000
  smolvm serve -v                      Enable verbose logging")]
pub struct ServeCmd {
    /// Address and port to listen on
    #[arg(
        short,
        long,
        default_value = "127.0.0.1:8080",
        value_name = "ADDR:PORT"
    )]
    listen: String,

    /// Enable debug logging (or set RUST_LOG=debug)
    #[arg(short, long)]
    verbose: bool,
}

impl ServeCmd {
    /// Run the serve command.
    pub fn run(self) -> Result<()> {
        // Parse listen address
        let addr: SocketAddr = self.listen.parse().map_err(|e| {
            smolvm::error::Error::Config(format!("invalid listen address '{}': {}", self.listen, e))
        })?;

        // Set up verbose logging if requested
        if self.verbose {
            // Re-initialize logging at debug level
            // Note: This won't work if logging is already initialized,
            // but the RUST_LOG env var can be used instead
            tracing::info!("verbose logging enabled");
        }

        // Create the runtime with signal handling enabled
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(smolvm::error::Error::Io)?;

        runtime.block_on(async move { self.run_server(addr).await })
    }

    async fn run_server(self, addr: SocketAddr) -> Result<()> {
        // Security warning if binding to all interfaces
        if addr.ip().is_unspecified() {
            eprintln!(
                "WARNING: Server is listening on all interfaces ({}).",
                addr.ip()
            );
            eprintln!("         The API has no authentication - any network client can control this host.");
            eprintln!("         Consider using --listen 127.0.0.1:8080 for local-only access.");
        }

        // Create shared state and load persisted sandboxes
        let state = Arc::new(ApiState::new().map_err(|e| {
            smolvm::error::Error::Config(format!("failed to initialize API state: {:?}", e))
        })?);
        let loaded = state.load_persisted_sandboxes();
        if !loaded.is_empty() {
            println!(
                "Reconnected to {} existing sandbox(es): {}",
                loaded.len(),
                loaded.join(", ")
            );
        }

        // Create shutdown channel for supervisor
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Spawn supervisor task
        let supervisor_state = state.clone();
        let supervisor_handle = tokio::spawn(async move {
            let supervisor =
                smolvm::api::supervisor::Supervisor::new(supervisor_state, shutdown_rx);
            supervisor.run().await;
        });

        // Create router
        let app = smolvm::api::create_router(state);

        // Create listener
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(smolvm::error::Error::Io)?;

        tracing::info!(address = %addr, "starting HTTP API server");
        println!("smolvm API server listening on http://{}", addr);

        // Run the server with graceful shutdown (VMs keep running independently)
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(smolvm::error::Error::Io)?;

        // Signal supervisor to stop
        let _ = shutdown_tx.send(true);

        // Wait for supervisor to finish (with timeout)
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), supervisor_handle).await;

        Ok(())
    }
}

/// Wait for shutdown signal.
/// Note: VMs are NOT stopped on server shutdown - they run independently.
/// Use DELETE /api/v1/sandboxes/:id to stop specific VMs.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
    eprintln!("\nShutting down server (VMs continue running)...");
}
