//! Buildah integration for OCI image management.
//!
//! This module provides functions for working with buildah to manage
//! OCI images and containers. Buildah handles image pulling, layer
//! management, and container filesystem mounting.

use crate::error::{Error, Result};
use std::path::PathBuf;
use std::process::Command;

/// Default policy.json content - accept all images.
const DEFAULT_POLICY_JSON: &str = r#"{"default":[{"type":"insecureAcceptAnything"}]}"#;

/// Default registries.conf content - search Docker Hub.
const DEFAULT_REGISTRIES_CONF: &str = r#"unqualified-search-registries = ["docker.io"]"#;

/// Get the smolvm config directory, creating it if needed.
fn get_config_dir() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("smolvm");

    if !config_dir.exists() {
        std::fs::create_dir_all(&config_dir)?;
    }

    Ok(config_dir)
}

/// Ensure container config files exist, creating defaults if needed.
fn ensure_container_configs() -> Result<(PathBuf, PathBuf)> {
    let config_dir = get_config_dir()?;

    let policy_path = config_dir.join("policy.json");
    if !policy_path.exists() {
        std::fs::write(&policy_path, DEFAULT_POLICY_JSON)?;
    }

    let registries_path = config_dir.join("registries.conf");
    if !registries_path.exists() {
        std::fs::write(&registries_path, DEFAULT_REGISTRIES_CONF)?;
    }

    Ok((policy_path, registries_path))
}

/// Get the storage volume path for macOS.
#[cfg(target_os = "macos")]
fn get_storage_volume() -> Option<String> {
    std::env::var("SMOLVM_STORAGE").ok()
}

/// Buildah command type.
enum BuildahCmd {
    From,
    Mount,
    Unmount,
    Remove,
    List,
}

/// Build a buildah command with platform-specific arguments.
fn buildah_command(cmd_type: BuildahCmd) -> Result<Command> {
    let mut cmd = Command::new("buildah");

    #[cfg(target_os = "macos")]
    {
        // Storage root on case-sensitive volume
        if let Some(volume) = get_storage_volume() {
            cmd.args(["--root", &format!("{}/root", volume)]);
            cmd.args(["--runroot", &format!("{}/runroot", volume)]);
        }

        // For 'from' command, pass policy and registries config
        if matches!(cmd_type, BuildahCmd::From) {
            let (policy_path, registries_path) = ensure_container_configs()?;
            cmd.arg("--signature-policy");
            cmd.arg(&policy_path);
            cmd.arg("--registries-conf");
            cmd.arg(&registries_path);
        }
    }

    // Add the actual command
    match cmd_type {
        BuildahCmd::From => {
            cmd.arg("from");
            #[cfg(target_os = "macos")]
            {
                cmd.args(["--os", "linux"]);
            }
        }
        BuildahCmd::Mount => {
            cmd.arg("mount");
        }
        BuildahCmd::Unmount => {
            cmd.arg("umount");
        }
        BuildahCmd::Remove => {
            cmd.arg("rm");
        }
        BuildahCmd::List => {
            cmd.args(["containers", "--format", "{{.ContainerID}}"]);
        }
    }

    Ok(cmd)
}

/// Create a container from an OCI image.
///
/// This pulls the image if necessary and creates a new container.
/// Returns the container ID.
///
/// # Arguments
///
/// * `image` - OCI image reference (e.g., "alpine:latest", "docker.io/library/alpine")
///
/// # Errors
///
/// Returns an error if buildah is not installed or the image cannot be pulled.
pub fn create_container(image: &str) -> Result<String> {
    tracing::info!(image = %image, "creating container from image");

    let output = buildah_command(BuildahCmd::From)?
        .arg(image)
        .output()
        .map_err(|e| Error::command_failed("buildah from", e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Rootfs(format!(
            "failed to create container from {}: {}",
            image,
            stderr.trim()
        )));
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    tracing::debug!(container_id = %container_id, "created container");

    Ok(container_id)
}

/// Mount a container and return the rootfs path.
///
/// # Arguments
///
/// * `container_id` - The buildah container ID
///
/// # Errors
///
/// Returns an error if the container cannot be mounted.
pub fn mount_container(container_id: &str) -> Result<PathBuf> {
    tracing::debug!(container_id = %container_id, "mounting container");

    let output = buildah_command(BuildahCmd::Mount)?
        .arg(container_id)
        .output()
        .map_err(|e| Error::command_failed("buildah mount", e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Rootfs(format!(
            "failed to mount container {}: {}",
            container_id,
            stderr.trim()
        )));
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    tracing::debug!(path = %path, "mounted container");

    // On macOS, fix root mode for overlay filesystem support
    #[cfg(target_os = "macos")]
    fix_root_mode(&path)?;

    Ok(PathBuf::from(path))
}

/// Fix root mode on macOS using xattr.
///
/// This sets extended attributes needed for overlay filesystem support.
#[cfg(target_os = "macos")]
fn fix_root_mode(rootfs: &str) -> Result<()> {
    let output = Command::new("xattr")
        .args(["-w", "user.containers.override_stat", "0:0:0555", rootfs])
        .output()
        .map_err(|e| Error::command_failed("xattr", e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(rootfs = %rootfs, error = %stderr, "failed to fix root mode");
    }

    Ok(())
}

/// Unmount a container.
///
/// # Arguments
///
/// * `container_id` - The buildah container ID
///
/// # Errors
///
/// Returns an error if the container cannot be unmounted.
pub fn unmount_container(container_id: &str) -> Result<()> {
    tracing::debug!(container_id = %container_id, "unmounting container");

    let output = buildah_command(BuildahCmd::Unmount)?
        .arg(container_id)
        .output()
        .map_err(|e| Error::command_failed("buildah unmount", e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Rootfs(format!(
            "failed to unmount container {}: {}",
            container_id,
            stderr.trim()
        )));
    }

    Ok(())
}

/// Remove a container.
///
/// # Arguments
///
/// * `container_id` - The buildah container ID
///
/// # Errors
///
/// Returns an error if the container cannot be removed.
pub fn remove_container(container_id: &str) -> Result<()> {
    tracing::debug!(container_id = %container_id, "removing container");

    // First unmount if mounted
    let _ = unmount_container(container_id);

    let output = buildah_command(BuildahCmd::Remove)?
        .arg(container_id)
        .output()
        .map_err(|e| Error::command_failed("buildah rm", e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Rootfs(format!(
            "failed to remove container {}: {}",
            container_id,
            stderr.trim()
        )));
    }

    Ok(())
}

/// List all buildah containers.
///
/// Returns a list of container IDs.
pub fn list_containers() -> Result<Vec<String>> {
    let output = buildah_command(BuildahCmd::List)?
        .output()
        .map_err(|e| Error::command_failed("buildah containers", e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Rootfs(format!(
            "failed to list containers: {}",
            stderr.trim()
        )));
    }

    let containers: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    Ok(containers)
}

/// Check if buildah is available.
pub fn is_available() -> bool {
    Command::new("buildah")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buildah_command_creation() {
        let cmd = buildah_command(BuildahCmd::Mount).unwrap();
        assert_eq!(cmd.get_program(), "buildah");
    }
}
