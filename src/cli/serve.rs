//! HTTP API server command.

use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;

use smolvm::api::state::ApiState;
use smolvm::Result;

/// Start the HTTP API server.
#[derive(Parser, Debug)]
pub struct ServeCmd {
    /// Listen address.
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    listen: String,

    /// Enable verbose logging.
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

        // Create the runtime and run the server
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| smolvm::error::Error::Io(e))?;

        runtime.block_on(async move {
            self.run_server(addr).await
        })
    }

    async fn run_server(self, addr: SocketAddr) -> Result<()> {
        // Security warning if binding to all interfaces
        if addr.ip().is_unspecified() {
            eprintln!("WARNING: Server is listening on all interfaces ({}).", addr.ip());
            eprintln!("         The API has no authentication - any network client can control this host.");
            eprintln!("         Consider using --listen 127.0.0.1:8080 for local-only access.");
        }

        // Create shared state and load persisted sandboxes
        let state = Arc::new(ApiState::new());
        let loaded = state.load_persisted_sandboxes();
        if !loaded.is_empty() {
            println!("Reconnected to {} existing sandbox(es): {}", loaded.len(), loaded.join(", "));
        }

        // Create router
        let app = smolvm::api::create_router(state);

        // Create listener
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| smolvm::error::Error::Io(e))?;

        tracing::info!(address = %addr, "starting HTTP API server");
        println!("smolvm API server listening on http://{}", addr);

        // Run the server with graceful shutdown (VMs keep running independently)
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| smolvm::error::Error::Io(e))?;

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
