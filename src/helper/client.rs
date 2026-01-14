//! vsock client for communicating with the helper daemon.
//!
//! This module provides a client for sending requests to the helper daemon
//! and receiving responses.

use crate::error::{Error, Result};
use crate::protocol::{
    encode_message, HelperRequest, HelperResponse, ImageInfo, OverlayInfo, StorageStatus,
};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// Client for communicating with the helper daemon.
pub struct HelperClient {
    stream: UnixStream,
}

impl HelperClient {
    /// Connect to the helper daemon via Unix socket.
    ///
    /// # Arguments
    ///
    /// * `socket_path` - Path to the vsock Unix socket
    pub fn connect(socket_path: impl AsRef<Path>) -> Result<Self> {
        let stream = UnixStream::connect(socket_path.as_ref()).map_err(|e| {
            Error::HelperError(format!("failed to connect to helper: {}", e))
        })?;

        // Set timeouts
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .ok();

        Ok(Self { stream })
    }

    /// Send a request and receive a response.
    fn request(&mut self, req: &HelperRequest) -> Result<HelperResponse> {
        // Encode and send request
        let data = encode_message(req).map_err(|e| Error::HelperError(e.to_string()))?;
        self.stream
            .write_all(&data)
            .map_err(|e| Error::HelperError(format!("write failed: {}", e)))?;

        // Read response
        self.read_response()
    }

    /// Read a response from the stream.
    fn read_response(&mut self) -> Result<HelperResponse> {
        // Read length header
        let mut header = [0u8; 4];
        self.stream
            .read_exact(&mut header)
            .map_err(|e| Error::HelperError(format!("read header failed: {}", e)))?;

        let len = u32::from_be_bytes(header) as usize;

        // Read payload
        let mut buf = vec![0u8; len];
        self.stream
            .read_exact(&mut buf)
            .map_err(|e| Error::HelperError(format!("read payload failed: {}", e)))?;

        // Parse response
        serde_json::from_slice(&buf).map_err(|e| Error::HelperError(format!("parse failed: {}", e)))
    }

    /// Ping the helper daemon.
    pub fn ping(&mut self) -> Result<u32> {
        let resp = self.request(&HelperRequest::Ping)?;

        match resp {
            HelperResponse::Pong { version } => Ok(version),
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }

    /// Pull an OCI image.
    ///
    /// # Arguments
    ///
    /// * `image` - Image reference (e.g., "alpine:latest")
    /// * `platform` - Optional platform (e.g., "linux/arm64")
    pub fn pull(&mut self, image: &str, platform: Option<&str>) -> Result<ImageInfo> {
        let resp = self.request(&HelperRequest::Pull {
            image: image.to_string(),
            platform: platform.map(String::from),
        })?;

        match resp {
            HelperResponse::Ok { data: Some(data) } => {
                serde_json::from_value(data).map_err(|e| Error::HelperError(e.to_string()))
            }
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }

    /// Query if an image exists locally.
    pub fn query(&mut self, image: &str) -> Result<Option<ImageInfo>> {
        let resp = self.request(&HelperRequest::Query {
            image: image.to_string(),
        })?;

        match resp {
            HelperResponse::Ok { data: Some(data) } => {
                let info: ImageInfo =
                    serde_json::from_value(data).map_err(|e| Error::HelperError(e.to_string()))?;
                Ok(Some(info))
            }
            HelperResponse::Error { code, .. } if code.as_deref() == Some("NOT_FOUND") => Ok(None),
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }

    /// List all cached images.
    pub fn list_images(&mut self) -> Result<Vec<ImageInfo>> {
        let resp = self.request(&HelperRequest::ListImages)?;

        match resp {
            HelperResponse::Ok { data: Some(data) } => {
                serde_json::from_value(data).map_err(|e| Error::HelperError(e.to_string()))
            }
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }

    /// Run garbage collection.
    ///
    /// # Arguments
    ///
    /// * `dry_run` - If true, only report what would be deleted
    pub fn garbage_collect(&mut self, dry_run: bool) -> Result<u64> {
        let resp = self.request(&HelperRequest::GarbageCollect { dry_run })?;

        match resp {
            HelperResponse::Ok { data: Some(data) } => {
                let freed = data["freed_bytes"].as_u64().unwrap_or(0);
                Ok(freed)
            }
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }

    /// Prepare an overlay filesystem for a workload.
    ///
    /// # Arguments
    ///
    /// * `image` - Image reference
    /// * `workload_id` - Unique workload identifier
    pub fn prepare_overlay(&mut self, image: &str, workload_id: &str) -> Result<OverlayInfo> {
        let resp = self.request(&HelperRequest::PrepareOverlay {
            image: image.to_string(),
            workload_id: workload_id.to_string(),
        })?;

        match resp {
            HelperResponse::Ok { data: Some(data) } => {
                serde_json::from_value(data).map_err(|e| Error::HelperError(e.to_string()))
            }
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }

    /// Clean up an overlay filesystem.
    pub fn cleanup_overlay(&mut self, workload_id: &str) -> Result<()> {
        let resp = self.request(&HelperRequest::CleanupOverlay {
            workload_id: workload_id.to_string(),
        })?;

        match resp {
            HelperResponse::Ok { .. } => Ok(()),
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }

    /// Format the storage disk.
    pub fn format_storage(&mut self) -> Result<()> {
        let resp = self.request(&HelperRequest::FormatStorage)?;

        match resp {
            HelperResponse::Ok { .. } => Ok(()),
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }

    /// Get storage status.
    pub fn storage_status(&mut self) -> Result<StorageStatus> {
        let resp = self.request(&HelperRequest::StorageStatus)?;

        match resp {
            HelperResponse::Ok { data: Some(data) } => {
                serde_json::from_value(data).map_err(|e| Error::HelperError(e.to_string()))
            }
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }

    /// Request helper shutdown.
    pub fn shutdown(&mut self) -> Result<()> {
        let resp = self.request(&HelperRequest::Shutdown)?;

        match resp {
            HelperResponse::Ok { .. } => Ok(()),
            HelperResponse::Error { message, .. } => Err(Error::HelperError(message)),
            _ => Err(Error::HelperError("unexpected response".into())),
        }
    }
}
