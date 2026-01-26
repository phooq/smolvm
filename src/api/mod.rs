//! HTTP API server for smolvm.
//!
//! This module provides an HTTP API for managing sandboxes, containers, and images
//! without CLI overhead.
//!
//! # Example
//!
//! ```bash
//! # Start the server
//! smolvm serve --listen 127.0.0.1:8080
//!
//! # Create a sandbox
//! curl -X POST http://localhost:8080/api/v1/sandboxes \
//!   -H "Content-Type: application/json" \
//!   -d '{"name": "test"}'
//! ```

pub mod error;
pub mod handlers;
pub mod state;
pub mod supervisor;
pub mod types;

use axum::{
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;
use tower_http::{cors::CorsLayer, timeout::TimeoutLayer, trace::TraceLayer};

use state::ApiState;

/// Create the API router with all endpoints.
pub fn create_router(state: Arc<ApiState>) -> Router {
    // Health check route
    let health_route = Router::new().route("/health", get(handlers::health::health));

    // SSE logs route (no timeout - streams indefinitely)
    let logs_route = Router::new().route("/:id/logs", get(handlers::exec::stream_logs));

    // Sandbox routes with timeout
    let sandbox_routes_with_timeout = Router::new()
        .route("/", post(handlers::sandboxes::create_sandbox))
        .route("/", get(handlers::sandboxes::list_sandboxes))
        .route("/:id", get(handlers::sandboxes::get_sandbox))
        .route("/:id/start", post(handlers::sandboxes::start_sandbox))
        .route("/:id/stop", post(handlers::sandboxes::stop_sandbox))
        .route("/:id", delete(handlers::sandboxes::delete_sandbox))
        // Exec routes
        .route("/:id/exec", post(handlers::exec::exec_command))
        .route("/:id/run", post(handlers::exec::run_command))
        // Container routes
        .route(
            "/:id/containers",
            post(handlers::containers::create_container),
        )
        .route(
            "/:id/containers",
            get(handlers::containers::list_containers),
        )
        .route(
            "/:id/containers/:cid/start",
            post(handlers::containers::start_container),
        )
        .route(
            "/:id/containers/:cid/stop",
            post(handlers::containers::stop_container),
        )
        .route(
            "/:id/containers/:cid",
            delete(handlers::containers::delete_container),
        )
        .route(
            "/:id/containers/:cid/exec",
            post(handlers::containers::exec_in_container),
        )
        // Image routes
        .route("/:id/images", get(handlers::images::list_images))
        .route("/:id/images/pull", post(handlers::images::pull_image))
        // Apply timeout only to these routes
        .layer(TimeoutLayer::new(std::time::Duration::from_secs(300)));

    // Combine sandbox routes (with and without timeout)
    let sandbox_routes = Router::new()
        .merge(logs_route)
        .merge(sandbox_routes_with_timeout);

    // API v1 routes
    let api_v1 = Router::new().nest("/sandboxes", sandbox_routes);

    // CORS: Allow localhost origins only by default for security.
    // Production deployments should configure their own CORS policy.
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:8080".parse().unwrap(),
            "http://127.0.0.1:8080".parse().unwrap(),
            "http://localhost:3000".parse().unwrap(),
            "http://127.0.0.1:3000".parse().unwrap(),
        ])
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
        ])
        .allow_headers([axum::http::header::CONTENT_TYPE]);

    // Combine all routes
    Router::new()
        .merge(health_route)
        .nest("/api/v1", api_v1)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}
