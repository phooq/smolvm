//! Root filesystem management.
//!
//! This module provides abstractions for preparing and managing guest root filesystems.
//! Currently supports:
//! - Direct path to a rootfs directory
//! - Buildah-managed containers (for OCI images)

pub mod buildah;

use crate::error::Result;
use std::path::PathBuf;

/// A prepared root filesystem.
///
/// This trait abstracts over different rootfs sources, providing a uniform
/// interface for accessing the mounted filesystem path.
pub trait Rootfs: Send {
    /// Get the path to the mounted rootfs.
    fn path(&self) -> &PathBuf;

    /// Cleanup the rootfs (unmount, etc.).
    fn cleanup(&mut self) -> Result<()>;
}

/// A simple path-based rootfs (no cleanup needed).
pub struct PathRootfs {
    path: PathBuf,
}

impl PathRootfs {
    /// Create a new path-based rootfs.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Rootfs for PathRootfs {
    fn path(&self) -> &PathBuf {
        &self.path
    }

    fn cleanup(&mut self) -> Result<()> {
        // Nothing to cleanup for a simple path
        Ok(())
    }
}

/// A buildah-managed rootfs.
pub struct BuildahRootfs {
    path: PathBuf,
    container_id: String,
}

impl BuildahRootfs {
    /// Create a new buildah-managed rootfs.
    ///
    /// This mounts the container and returns the rootfs path.
    pub fn new(container_id: impl Into<String>) -> Result<Self> {
        let container_id = container_id.into();
        let path = buildah::mount_container(&container_id)?;
        Ok(Self { path, container_id })
    }

    /// Get the container ID.
    pub fn container_id(&self) -> &str {
        &self.container_id
    }
}

impl Rootfs for BuildahRootfs {
    fn path(&self) -> &PathBuf {
        &self.path
    }

    fn cleanup(&mut self) -> Result<()> {
        buildah::unmount_container(&self.container_id)
    }
}

impl Drop for BuildahRootfs {
    fn drop(&mut self) {
        if let Err(e) = self.cleanup() {
            tracing::warn!(
                "failed to cleanup buildah rootfs for container {}: {}",
                self.container_id,
                e
            );
        }
    }
}
