//! Shared CLI argument parsers.
//!
//! This module consolidates parser functions used across multiple CLI commands
//! to eliminate code duplication and ensure consistent validation.

use smolvm::agent::PortMapping;
use smolvm::vm::config::HostMount;
use smolvm::Error;
use std::path::PathBuf;
use std::time::Duration;

/// Parse a duration string (e.g., "30s", "5m", "1h").
pub fn parse_duration(s: &str) -> Result<Duration, humantime::DurationError> {
    humantime::parse_duration(s)
}

/// Parse a port mapping specification (HOST:GUEST or PORT).
pub fn parse_port(s: &str) -> Result<PortMapping, String> {
    if let Some((host, guest)) = s.split_once(':') {
        let host: u16 = host
            .parse()
            .map_err(|_| format!("invalid host port: {}", host))?;
        let guest: u16 = guest
            .parse()
            .map_err(|_| format!("invalid guest port: {}", guest))?;
        Ok(PortMapping::new(host, guest))
    } else {
        let port: u16 = s.parse().map_err(|_| format!("invalid port: {}", s))?;
        Ok(PortMapping::same(port))
    }
}

/// Parse an environment variable specification (KEY=VALUE).
pub fn parse_env_spec(spec: &str) -> Option<(String, String)> {
    let (key, value) = spec.split_once('=')?;
    if key.is_empty() {
        None
    } else {
        Some((key.to_string(), value.to_string()))
    }
}

/// Parse environment variables from CLI args.
pub fn parse_env_list(env_args: &[String]) -> Vec<(String, String)> {
    env_args.iter().filter_map(|e| parse_env_spec(e)).collect()
}

/// Parse volume mount specifications into HostMount structs.
///
/// Format: `host_path:container_path[:ro]`
///
/// Validates that the host path exists and is a directory.
pub fn parse_mounts(specs: &[String]) -> smolvm::Result<Vec<HostMount>> {
    specs.iter().map(|spec| parse_mount_spec(spec)).collect()
}

/// Parse a single mount specification.
fn parse_mount_spec(spec: &str) -> smolvm::Result<HostMount> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() < 2 {
        return Err(Error::Mount(format!(
            "invalid volume specification '{}': expected host:container[:ro]",
            spec
        )));
    }

    let host_path = PathBuf::from(parts[0]);
    let guest_path = PathBuf::from(parts[1]);
    let read_only = parts.get(2).map(|&s| s == "ro").unwrap_or(false);

    // Validate host path exists
    if !host_path.exists() {
        return Err(Error::Mount(format!(
            "host path does not exist: {}",
            host_path.display()
        )));
    }

    // Must be a directory (virtiofs limitation)
    if !host_path.is_dir() {
        return Err(Error::Mount(format!(
            "host path must be a directory (virtiofs limitation): {}",
            host_path.display()
        )));
    }

    // Canonicalize host path
    let host_path = host_path
        .canonicalize()
        .map_err(|e| Error::Mount(format!("failed to resolve host path '{}': {}", parts[0], e)))?;

    Ok(if read_only {
        HostMount::new(host_path, guest_path)
    } else {
        HostMount::new_writable(host_path, guest_path)
    })
}

/// Parse mounts and convert to tuple format for database storage.
pub fn parse_mounts_as_tuples(specs: &[String]) -> smolvm::Result<Vec<(String, String, bool)>> {
    parse_mounts(specs).map(|mounts| {
        mounts
            .into_iter()
            .map(|m| {
                (
                    m.source.to_string_lossy().to_string(),
                    m.target.to_string_lossy().to_string(),
                    m.read_only,
                )
            })
            .collect()
    })
}

/// Parse mounts and convert to virtiofs binding format for agent.
///
/// Returns tuples of (virtiofs_tag, container_path, read_only).
pub fn parse_mounts_to_bindings(specs: &[String]) -> smolvm::Result<Vec<(String, String, bool)>> {
    parse_mounts(specs).map(|mounts| mounts_to_virtiofs_bindings(&mounts))
}

/// Convert parsed HostMount list to virtiofs binding format for agent.
///
/// Returns tuples of (virtiofs_tag, container_path, read_only).
/// The tag format is "smolvm{index}" to match libkrun virtiofs device naming.
pub fn mounts_to_virtiofs_bindings(mounts: &[HostMount]) -> Vec<(String, String, bool)> {
    mounts
        .iter()
        .enumerate()
        .map(|(i, m)| {
            (
                format!("smolvm{}", i),
                m.target.to_string_lossy().to_string(),
                m.read_only,
            )
        })
        .collect()
}
