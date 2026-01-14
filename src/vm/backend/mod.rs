//! VM backend implementations.
//!
//! This module provides hypervisor backend implementations for different platforms.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod libkrun;

use crate::error::{Error, Result};
use crate::vm::VmBackend;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use libkrun::LibkrunBackend;

/// Create the default backend for this platform.
pub fn create_default() -> Result<Box<dyn VmBackend>> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let backend = LibkrunBackend::new()?;
        if backend.is_available() {
            return Ok(Box::new(backend));
        }
    }

    Err(Error::HypervisorUnavailable(
        "no available backend for this platform".into(),
    ))
}

/// List all available backends.
pub fn available_backends() -> Vec<&'static str> {
    let mut backends = Vec::new();

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if LibkrunBackend::new().map(|b| b.is_available()).unwrap_or(false) {
            backends.push("libkrun");
        }
    }

    backends
}
