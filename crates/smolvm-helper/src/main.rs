//! smolvm helper daemon.
//!
//! This daemon runs inside the helper VM and handles:
//! - OCI image pulling via crane
//! - Layer extraction and storage management
//! - Overlay filesystem preparation for workloads
//!
//! Communication is via vsock on port 6000.

use smolvm_protocol::{
    ports, DecodeError, HelperRequest, HelperResponse, ImageInfo, OverlayInfo, StorageStatus,
    PROTOCOL_VERSION,
};
use std::io::{Read, Write};
use tracing::{debug, error, info, warn};

mod storage;
mod vsock;

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("smolvm_helper=debug".parse().unwrap()),
        )
        .init();

    info!(version = env!("CARGO_PKG_VERSION"), "starting smolvm-helper");

    // Initialize storage
    if let Err(e) = storage::init() {
        error!(error = %e, "failed to initialize storage");
        std::process::exit(1);
    }

    // Start vsock server
    if let Err(e) = run_server() {
        error!(error = %e, "server error");
        std::process::exit(1);
    }
}

/// Run the vsock server.
fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let listener = vsock::listen(ports::HELPER_CONTROL)?;
    info!(port = ports::HELPER_CONTROL, "listening on vsock");

    loop {
        match listener.accept() {
            Ok(mut stream) => {
                info!("accepted connection");

                if let Err(e) = handle_connection(&mut stream) {
                    warn!(error = %e, "connection error");
                }
            }
            Err(e) => {
                warn!(error = %e, "accept error");
            }
        }
    }
}

/// Handle a single connection.
fn handle_connection(stream: &mut impl ReadWrite) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = vec![0u8; 64 * 1024]; // 64KB buffer

    loop {
        // Read length header
        let mut header = [0u8; 4];
        match stream.read_exact(&mut header) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                debug!("connection closed");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }

        let len = u32::from_be_bytes(header) as usize;
        if len > buf.len() {
            buf.resize(len, 0);
        }

        // Read payload
        stream.read_exact(&mut buf[..len])?;

        // Parse request
        let request: HelperRequest = match serde_json::from_slice(&buf[..len]) {
            Ok(req) => req,
            Err(e) => {
                warn!(error = %e, "invalid request");
                send_response(stream, &HelperResponse::Error {
                    message: format!("invalid request: {}", e),
                    code: Some("INVALID_REQUEST".to_string()),
                })?;
                continue;
            }
        };

        debug!(?request, "received request");

        // Handle request
        let response = handle_request(request);
        send_response(stream, &response)?;

        // Check for shutdown
        if matches!(response, HelperResponse::Ok { .. }) {
            // If this was a shutdown request, exit
            if let HelperResponse::Ok { data: Some(ref d) } = response {
                if d.get("shutdown").and_then(|v| v.as_bool()) == Some(true) {
                    info!("shutdown requested");
                    return Ok(());
                }
            }
        }
    }
}

/// Handle a single request.
fn handle_request(request: HelperRequest) -> HelperResponse {
    match request {
        HelperRequest::Ping => HelperResponse::Pong {
            version: PROTOCOL_VERSION,
        },

        HelperRequest::Pull { image, platform } => handle_pull(&image, platform.as_deref()),

        HelperRequest::Query { image } => handle_query(&image),

        HelperRequest::ListImages => handle_list_images(),

        HelperRequest::GarbageCollect { dry_run } => handle_gc(dry_run),

        HelperRequest::PrepareOverlay { image, workload_id } => {
            handle_prepare_overlay(&image, &workload_id)
        }

        HelperRequest::CleanupOverlay { workload_id } => handle_cleanup_overlay(&workload_id),

        HelperRequest::FormatStorage => handle_format_storage(),

        HelperRequest::StorageStatus => handle_storage_status(),

        HelperRequest::Shutdown => {
            info!("shutdown requested");
            HelperResponse::Ok {
                data: Some(serde_json::json!({"shutdown": true})),
            }
        }
    }
}

/// Handle image pull request.
fn handle_pull(image: &str, platform: Option<&str>) -> HelperResponse {
    info!(image = %image, ?platform, "pulling image");

    match storage::pull_image(image, platform) {
        Ok(info) => HelperResponse::Ok {
            data: Some(serde_json::to_value(info).unwrap()),
        },
        Err(e) => HelperResponse::Error {
            message: e.to_string(),
            code: Some("PULL_FAILED".to_string()),
        },
    }
}

/// Handle image query request.
fn handle_query(image: &str) -> HelperResponse {
    match storage::query_image(image) {
        Ok(Some(info)) => HelperResponse::Ok {
            data: Some(serde_json::to_value(info).unwrap()),
        },
        Ok(None) => HelperResponse::Error {
            message: format!("image not found: {}", image),
            code: Some("NOT_FOUND".to_string()),
        },
        Err(e) => HelperResponse::Error {
            message: e.to_string(),
            code: Some("QUERY_FAILED".to_string()),
        },
    }
}

/// Handle list images request.
fn handle_list_images() -> HelperResponse {
    match storage::list_images() {
        Ok(images) => HelperResponse::Ok {
            data: Some(serde_json::to_value(images).unwrap()),
        },
        Err(e) => HelperResponse::Error {
            message: e.to_string(),
            code: Some("LIST_FAILED".to_string()),
        },
    }
}

/// Handle garbage collection request.
fn handle_gc(dry_run: bool) -> HelperResponse {
    match storage::garbage_collect(dry_run) {
        Ok(freed) => HelperResponse::Ok {
            data: Some(serde_json::json!({
                "freed_bytes": freed,
                "dry_run": dry_run,
            })),
        },
        Err(e) => HelperResponse::Error {
            message: e.to_string(),
            code: Some("GC_FAILED".to_string()),
        },
    }
}

/// Handle overlay preparation request.
fn handle_prepare_overlay(image: &str, workload_id: &str) -> HelperResponse {
    info!(image = %image, workload_id = %workload_id, "preparing overlay");

    match storage::prepare_overlay(image, workload_id) {
        Ok(info) => HelperResponse::Ok {
            data: Some(serde_json::to_value(info).unwrap()),
        },
        Err(e) => HelperResponse::Error {
            message: e.to_string(),
            code: Some("OVERLAY_FAILED".to_string()),
        },
    }
}

/// Handle overlay cleanup request.
fn handle_cleanup_overlay(workload_id: &str) -> HelperResponse {
    info!(workload_id = %workload_id, "cleaning up overlay");

    match storage::cleanup_overlay(workload_id) {
        Ok(_) => HelperResponse::Ok { data: None },
        Err(e) => HelperResponse::Error {
            message: e.to_string(),
            code: Some("CLEANUP_FAILED".to_string()),
        },
    }
}

/// Handle storage format request.
fn handle_format_storage() -> HelperResponse {
    info!("formatting storage");

    match storage::format() {
        Ok(_) => HelperResponse::Ok { data: None },
        Err(e) => HelperResponse::Error {
            message: e.to_string(),
            code: Some("FORMAT_FAILED".to_string()),
        },
    }
}

/// Handle storage status request.
fn handle_storage_status() -> HelperResponse {
    match storage::status() {
        Ok(status) => HelperResponse::Ok {
            data: Some(serde_json::to_value(status).unwrap()),
        },
        Err(e) => HelperResponse::Error {
            message: e.to_string(),
            code: Some("STATUS_FAILED".to_string()),
        },
    }
}

/// Send a response to the client.
fn send_response(
    stream: &mut impl Write,
    response: &HelperResponse,
) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_vec(response)?;
    let len = json.len() as u32;

    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(&json)?;
    stream.flush()?;

    debug!(?response, "sent response");
    Ok(())
}

/// Trait for read+write streams.
trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}
