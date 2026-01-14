//! Persistent storage management.
//!
//! This module provides types and utilities for managing persistent
//! writable disks for VMs and shared OCI layer storage.
//!
//! # Storage Types
//!
//! - [`StorageDisk`]: Shared disk for OCI layer storage (used by helper VM)
//! - [`WritableDisk`]: Per-VM writable overlay disk
//!
//! # Architecture
//!
//! The storage disk is a sparse raw disk image formatted with ext4.
//! It's mounted inside the helper VM which handles OCI layer extraction
//! and overlay filesystem management.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Default size for the shared storage disk (20 GB sparse).
pub const DEFAULT_STORAGE_SIZE_GB: u64 = 20;

/// Storage disk filename.
pub const STORAGE_DISK_FILENAME: &str = "storage.raw";

/// Marker file indicating disk has been formatted.
const FORMATTED_MARKER: &str = ".smolvm_formatted";

/// Disk format version info (stored at `/.smolvm/version.json` in ext4 disk).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskVersion {
    /// Format version (currently: 1).
    pub format_version: u32,

    /// Timestamp when the disk was created.
    pub created_at: String,

    /// Digest of the base rootfs image.
    pub base_digest: String,

    /// smolvm version that created this disk.
    pub smolvm_version: String,
}

impl DiskVersion {
    /// Current format version.
    pub const CURRENT_VERSION: u32 = 1;

    /// Create a new disk version with current settings.
    pub fn new(base_digest: impl Into<String>) -> Self {
        Self {
            format_version: Self::CURRENT_VERSION,
            created_at: current_timestamp(),
            base_digest: base_digest.into(),
            smolvm_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Check if this version is compatible with the current smolvm.
    pub fn is_compatible(&self) -> bool {
        self.format_version <= Self::CURRENT_VERSION
    }
}

/// Shared storage disk for OCI layers.
///
/// This is a sparse raw disk image that the helper VM mounts to store
/// OCI image layers and overlay filesystems.
///
/// # Directory Structure (inside ext4)
///
/// ```text
/// /
/// ├── .smolvm_formatted    # Marker file
/// ├── layers/              # Extracted OCI layers (content-addressed)
/// │   └── sha256:{digest}/ # Each layer as a directory
/// ├── configs/             # OCI image configs
/// │   └── {digest}.json
/// ├── overlays/            # Workload overlay directories
/// │   └── {workload_id}/
/// │       ├── upper/       # Writable layer
/// │       ├── work/        # Overlay work directory
/// │       └── merged/      # Mount point (optional)
/// └── manifests/           # Cached image manifests
///     └── {image_ref}.json
/// ```
#[derive(Debug, Clone)]
pub struct StorageDisk {
    /// Path to the disk image file.
    path: PathBuf,
    /// Size in bytes.
    size_bytes: u64,
}

impl StorageDisk {
    /// Get the default path for the storage disk.
    ///
    /// On macOS: `~/Library/Application Support/smolvm/storage.raw`
    /// On Linux: `~/.local/share/smolvm/storage.raw`
    pub fn default_path() -> Result<PathBuf> {
        let data_dir = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| Error::Storage("could not determine data directory".into()))?;

        let smolvm_dir = data_dir.join("smolvm");
        Ok(smolvm_dir.join(STORAGE_DISK_FILENAME))
    }

    /// Open or create the storage disk at the default location.
    pub fn open_or_create() -> Result<Self> {
        let path = Self::default_path()?;
        Self::open_or_create_at(&path, DEFAULT_STORAGE_SIZE_GB)
    }

    /// Open or create the storage disk at a custom path.
    pub fn open_or_create_at(path: &Path, size_gb: u64) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let size_bytes = size_gb * 1024 * 1024 * 1024;

        if path.exists() {
            // Open existing disk
            let metadata = std::fs::metadata(path)?;
            Ok(Self {
                path: path.to_path_buf(),
                size_bytes: metadata.len(),
            })
        } else {
            // Create sparse disk image
            Self::create_sparse(path, size_bytes)?;
            Ok(Self {
                path: path.to_path_buf(),
                size_bytes,
            })
        }
    }

    /// Create a sparse disk image.
    fn create_sparse(path: &Path, size_bytes: u64) -> Result<()> {
        use std::fs::OpenOptions;
        use std::io::{Seek, SeekFrom, Write};

        tracing::info!(path = %path.display(), size_gb = size_bytes / (1024*1024*1024), "creating sparse storage disk");

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;

        // Seek to end and write a single byte to create sparse file
        file.seek(SeekFrom::Start(size_bytes - 1))?;
        file.write_all(&[0])?;
        file.sync_all()?;

        Ok(())
    }

    /// Get the path to the disk image.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the disk size in bytes.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// Get the disk size in GB.
    pub fn size_gb(&self) -> u64 {
        self.size_bytes / (1024 * 1024 * 1024)
    }

    /// Check if the disk needs to be formatted.
    ///
    /// This checks for a marker file that's created after formatting.
    /// The actual formatting happens inside the helper VM.
    pub fn needs_format(&self) -> bool {
        // We can't check inside the ext4 filesystem from the host.
        // Instead, we use a sidecar file.
        let marker_path = self.marker_path();
        !marker_path.exists()
    }

    /// Mark the disk as formatted.
    ///
    /// This creates a marker file next to the disk image.
    pub fn mark_formatted(&self) -> Result<()> {
        let marker_path = self.marker_path();
        std::fs::write(&marker_path, "1")?;
        Ok(())
    }

    /// Get the path to the format marker file.
    fn marker_path(&self) -> PathBuf {
        self.path.with_extension("formatted")
    }

    /// Delete the storage disk and its marker.
    pub fn delete(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        let marker = self.marker_path();
        if marker.exists() {
            std::fs::remove_file(&marker)?;
        }
        Ok(())
    }
}

/// A writable disk image for VM state persistence.
#[derive(Debug, Clone)]
pub struct WritableDisk {
    /// Path to the disk image file.
    pub path: PathBuf,

    /// Disk version information.
    pub version: Option<DiskVersion>,

    /// Size in bytes.
    pub size_bytes: u64,
}

impl WritableDisk {
    /// Default disk size (10 GB).
    pub const DEFAULT_SIZE_MB: u64 = 10240;

    /// Create a new writable disk (Phase 1 implementation).
    ///
    /// This is a placeholder that will be implemented in Phase 1.
    pub fn create(
        _path: &std::path::Path,
        _size_mb: u64,
        _base_digest: &str,
    ) -> crate::error::Result<Self> {
        Err(crate::error::Error::Storage(
            "disk creation not yet implemented (Phase 1)".into(),
        ))
    }

    /// Open an existing writable disk (Phase 1 implementation).
    pub fn open(_path: &std::path::Path) -> crate::error::Result<Self> {
        Err(crate::error::Error::Storage(
            "disk open not yet implemented (Phase 1)".into(),
        ))
    }
}

/// Get current timestamp as a simple string.
fn current_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    format!("{}", duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disk_version_compatibility() {
        // Important for migration safety
        let version = DiskVersion::new("sha256:abc123");
        assert!(version.is_compatible());

        let future_version = DiskVersion {
            format_version: 999,
            created_at: "0".to_string(),
            base_digest: "sha256:abc123".to_string(),
            smolvm_version: "99.0.0".to_string(),
        };
        assert!(!future_version.is_compatible());
    }

    #[test]
    fn test_disk_version_serialization() {
        let version = DiskVersion::new("sha256:abc123");
        let json = serde_json::to_string(&version).unwrap();
        let deserialized: DiskVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.format_version, version.format_version);
        assert_eq!(deserialized.base_digest, version.base_digest);
    }

    #[test]
    fn test_storage_disk_create_and_delete() {
        let temp_dir = std::env::temp_dir().join("smolvm_test");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let disk_path = temp_dir.join("test_storage.raw");

        // Clean up any existing file
        let _ = std::fs::remove_file(&disk_path);
        let _ = std::fs::remove_file(disk_path.with_extension("formatted"));

        // Create a small disk for testing (1 GB)
        let disk = StorageDisk::open_or_create_at(&disk_path, 1).unwrap();

        assert!(disk_path.exists());
        assert_eq!(disk.size_gb(), 1);
        assert!(disk.needs_format());

        // Mark as formatted
        disk.mark_formatted().unwrap();
        assert!(!disk.needs_format());

        // Delete disk
        disk.delete().unwrap();
        assert!(!disk_path.exists());

        // Clean up temp dir
        let _ = std::fs::remove_dir(&temp_dir);
    }
}
