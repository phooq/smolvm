//! Protocol types for smolvm host-guest communication.
//!
//! This crate defines the wire protocol for vsock communication between
//! the smolvm host and guest VMs (helper daemon and workload VMs).
//!
//! # Protocol Overview
//!
//! Communication uses JSON-encoded messages over vsock. Each message is
//! prefixed with a 4-byte big-endian length header.
//!
//! ```text
//! +----------------+-------------------+
//! | Length (4 BE)  | JSON payload      |
//! +----------------+-------------------+
//! ```

#![deny(missing_docs)]

use serde::{Deserialize, Serialize};

/// Protocol version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum frame size (16 MB).
pub const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

/// Well-known vsock ports.
pub mod ports {
    /// Control channel for workload VMs.
    pub const WORKLOAD_CONTROL: u32 = 5000;
    /// Log streaming from workload VMs.
    pub const WORKLOAD_LOGS: u32 = 5001;
    /// Helper daemon control port.
    pub const HELPER_CONTROL: u32 = 6000;
}

/// vsock CID constants.
pub mod cid {
    /// Host CID (always 2).
    pub const HOST: u32 = 2;
    /// Guest CID (always 3 for the first/only guest).
    pub const GUEST: u32 = 3;
    /// Any CID (for listening).
    pub const ANY: u32 = u32::MAX;
}

// ============================================================================
// Helper Daemon Protocol
// ============================================================================

/// Helper daemon request types (for image management).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum HelperRequest {
    /// Ping to check if helper is alive.
    Ping,

    /// Pull an OCI image and extract layers.
    Pull {
        /// Image reference (e.g., "alpine:latest", "docker.io/library/ubuntu:22.04").
        image: String,
        /// Platform to pull (e.g., "linux/arm64", "linux/amd64").
        platform: Option<String>,
    },

    /// Query if an image exists locally.
    Query {
        /// Image reference.
        image: String,
    },

    /// List all cached images.
    ListImages,

    /// Run garbage collection on unused layers.
    GarbageCollect {
        /// If true, only report what would be deleted.
        dry_run: bool,
    },

    /// Prepare overlay rootfs for a workload.
    PrepareOverlay {
        /// Image reference.
        image: String,
        /// Unique workload ID for the overlay.
        workload_id: String,
    },

    /// Clean up overlay rootfs for a workload.
    CleanupOverlay {
        /// Workload ID to clean up.
        workload_id: String,
    },

    /// Format the storage disk (first-time setup).
    FormatStorage,

    /// Get storage disk status.
    StorageStatus,

    /// Shutdown the helper daemon.
    Shutdown,
}

/// Helper daemon response types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HelperResponse {
    /// Operation completed successfully.
    Ok {
        /// Response data (varies by request type).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
    },

    /// Pong response to ping.
    Pong {
        /// Protocol version.
        version: u32,
    },

    /// Progress update (for long operations like pull).
    Progress {
        /// Human-readable message.
        message: String,
        /// Completion percentage (0-100).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        percent: Option<u8>,
        /// Current layer being processed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layer: Option<String>,
    },

    /// Operation failed.
    Error {
        /// Error message.
        message: String,
        /// Error code (for programmatic handling).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

/// Image information returned by Query/ListImages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    /// Image reference.
    pub reference: String,
    /// Image digest (sha256:...).
    pub digest: String,
    /// Image size in bytes.
    pub size: u64,
    /// Creation timestamp (ISO 8601).
    pub created: Option<String>,
    /// Platform architecture.
    pub architecture: String,
    /// Platform OS.
    pub os: String,
    /// Number of layers.
    pub layer_count: usize,
    /// Layer digests in order.
    pub layers: Vec<String>,
}

/// Overlay preparation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayInfo {
    /// Path to the merged overlay rootfs.
    pub rootfs_path: String,
    /// Path to the upper (writable) directory.
    pub upper_path: String,
    /// Path to the work directory.
    pub work_path: String,
}

/// Storage status information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageStatus {
    /// Whether the storage is formatted and ready.
    pub ready: bool,
    /// Total size in bytes.
    pub total_bytes: u64,
    /// Used size in bytes.
    pub used_bytes: u64,
    /// Number of cached layers.
    pub layer_count: usize,
    /// Number of cached images.
    pub image_count: usize,
}

// ============================================================================
// Workload VM Protocol
// ============================================================================

/// Messages from host to workload VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// Authentication request.
    Auth {
        /// Authentication token (base64).
        token: String,
        /// Protocol version.
        protocol_version: u32,
    },

    /// Run a command.
    Run {
        /// Request ID for correlating responses.
        request_id: u64,
        /// Command and arguments.
        command: Vec<String>,
        /// Environment variables.
        env: Vec<(String, String)>,
        /// Working directory.
        workdir: Option<String>,
    },

    /// Execute a command in running VM.
    Exec {
        /// Request ID.
        request_id: u64,
        /// Command and arguments.
        command: Vec<String>,
        /// Allocate a TTY.
        tty: bool,
    },

    /// Send a signal to a running command.
    Signal {
        /// Request ID of the command.
        request_id: u64,
        /// Signal number.
        signal: i32,
    },

    /// Request graceful shutdown.
    Stop {
        /// Timeout in milliseconds.
        timeout_ms: u64,
    },
}

/// Messages from workload VM to host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuestMessage {
    /// Authentication successful.
    AuthOk,

    /// Authentication failed.
    AuthFailed,

    /// VM is ready to receive commands.
    Ready,

    /// Command started.
    Started {
        /// Request ID.
        request_id: u64,
    },

    /// Stdout data from command.
    Stdout {
        /// Request ID.
        request_id: u64,
        /// Output data.
        data: Vec<u8>,
        /// Whether output was truncated.
        truncated: bool,
    },

    /// Stderr data from command.
    Stderr {
        /// Request ID.
        request_id: u64,
        /// Output data.
        data: Vec<u8>,
        /// Whether output was truncated.
        truncated: bool,
    },

    /// Command exited.
    Exit {
        /// Request ID.
        request_id: u64,
        /// Exit code.
        code: i32,
        /// Exit reason.
        reason: String,
    },

    /// Error occurred.
    Error {
        /// Request ID (if applicable).
        request_id: Option<u64>,
        /// Error message.
        message: String,
    },
}

// ============================================================================
// Wire Format Helpers
// ============================================================================

/// Encode a message to wire format (length-prefixed JSON).
pub fn encode_message<T: Serialize>(msg: &T) -> Result<Vec<u8>, serde_json::Error> {
    let json = serde_json::to_vec(msg)?;
    let len = json.len() as u32;

    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&json);

    Ok(buf)
}

/// Decode a message from wire format.
pub fn decode_message<T: for<'de> Deserialize<'de>>(data: &[u8]) -> Result<T, DecodeError> {
    if data.len() < 4 {
        return Err(DecodeError::TooShort);
    }

    let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;

    if len > MAX_FRAME_SIZE as usize {
        return Err(DecodeError::TooLarge(len));
    }

    if data.len() < 4 + len {
        return Err(DecodeError::Incomplete {
            expected: len,
            got: data.len() - 4,
        });
    }

    serde_json::from_slice(&data[4..4 + len]).map_err(DecodeError::Json)
}

/// Error decoding a wire message.
#[derive(Debug)]
pub enum DecodeError {
    /// Data too short to contain length header.
    TooShort,
    /// Frame size exceeds maximum.
    TooLarge(usize),
    /// Incomplete frame.
    Incomplete {
        /// Expected length.
        expected: usize,
        /// Actual length.
        got: usize,
    },
    /// JSON parse error.
    Json(serde_json::Error),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooShort => write!(f, "data too short for length header"),
            DecodeError::TooLarge(size) => write!(f, "frame too large: {} bytes", size),
            DecodeError::Incomplete { expected, got } => {
                write!(f, "incomplete frame: expected {} bytes, got {}", expected, got)
            }
            DecodeError::Json(e) => write!(f, "JSON decode error: {}", e),
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let req = HelperRequest::Pull {
            image: "alpine:latest".to_string(),
            platform: Some("linux/arm64".to_string()),
        };

        let encoded = encode_message(&req).unwrap();
        let decoded: HelperRequest = decode_message(&encoded).unwrap();

        match decoded {
            HelperRequest::Pull { image, platform } => {
                assert_eq!(image, "alpine:latest");
                assert_eq!(platform, Some("linux/arm64".to_string()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_decode_too_short() {
        let data = [0u8; 2];
        let result: Result<HelperRequest, _> = decode_message(&data);
        assert!(matches!(result, Err(DecodeError::TooShort)));
    }

    #[test]
    fn test_decode_incomplete() {
        let mut data = vec![0, 0, 0, 100]; // claims 100 bytes
        data.extend_from_slice(b"{}"); // only 2 bytes of payload
        let result: Result<HelperRequest, _> = decode_message(&data);
        assert!(matches!(result, Err(DecodeError::Incomplete { .. })));
    }

    #[test]
    fn test_helper_request_serialization() {
        let req = HelperRequest::Ping;
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("ping"));

        let req = HelperRequest::PrepareOverlay {
            image: "ubuntu:22.04".to_string(),
            workload_id: "wl-123".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("prepare_overlay"));
    }

    #[test]
    fn test_helper_response_serialization() {
        let resp = HelperResponse::Pong {
            version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("pong"));

        let resp = HelperResponse::Progress {
            message: "Pulling layer 1/3".to_string(),
            percent: Some(33),
            layer: Some("sha256:abc123".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("progress"));
    }

    #[test]
    fn test_ports_constants() {
        assert_eq!(ports::WORKLOAD_CONTROL, 5000);
        assert_eq!(ports::WORKLOAD_LOGS, 5001);
        assert_eq!(ports::HELPER_CONTROL, 6000);
    }

    #[test]
    fn test_cid_constants() {
        assert_eq!(cid::HOST, 2);
        assert_eq!(cid::GUEST, 3);
    }
}
