//! Rosetta 2 support for running x86_64 binaries on Apple Silicon.
//!
//! This module provides detection and configuration for Apple's Rosetta 2
//! translation layer, allowing x86_64 container images to run on ARM Macs.
//!
//! # How it works
//!
//! When Rosetta is available and enabled, smolvm mounts the Rosetta runtime
//! directory into the guest VM via virtiofs. The guest can then execute
//! x86_64 binaries by registering Rosetta with binfmt_misc.
//!
//! # Requirements
//!
//! - Apple Silicon Mac (M1/M2/M3)
//! - Rosetta 2 installed (`softwareupdate --install-rosetta`)
//! - macOS 11.0 or later

use std::path::Path;

/// Virtiofs tag for the Rosetta mount.
pub const ROSETTA_TAG: &str = "rosetta";

/// Guest mount path for Rosetta runtime.
pub const ROSETTA_GUEST_PATH: &str = "/mnt/rosetta";

/// Path to the Rosetta runtime on macOS.
#[cfg(target_os = "macos")]
const ROSETTA_RUNTIME_PATH: &str = "/Library/Apple/usr/libexec/oah";

/// Check if Rosetta 2 is available on this system.
///
/// Returns `true` only on Apple Silicon Macs with Rosetta installed.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn is_available() -> bool {
    Path::new(ROSETTA_RUNTIME_PATH).exists()
        && Path::new("/Library/Apple/usr/libexec/oah/libRosettaRuntime").exists()
}

/// Check if Rosetta 2 is available (non-ARM or non-macOS).
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub fn is_available() -> bool {
    false
}

/// Get the path to the Rosetta runtime directory.
///
/// Returns `None` if Rosetta is not available.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn runtime_path() -> Option<&'static str> {
    if is_available() {
        Some(ROSETTA_RUNTIME_PATH)
    } else {
        None
    }
}

/// Get the path to the Rosetta runtime directory (non-ARM or non-macOS).
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub fn runtime_path() -> Option<&'static str> {
    None
}

/// binfmt_misc registration command for the guest.
///
/// This command should be run inside the guest VM to register Rosetta
/// as the interpreter for x86_64 ELF binaries.
pub const BINFMT_REGISTER_CMD: &str = r#"
if [ -d /mnt/rosetta ] && [ -f /mnt/rosetta/rosetta ]; then
    if [ -d /proc/sys/fs/binfmt_misc ]; then
        mount -t binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc 2>/dev/null || true
        echo ':rosetta:M::\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00:\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff:/mnt/rosetta/rosetta:OCF' > /proc/sys/fs/binfmt_misc/register 2>/dev/null || true
    fi
fi
"#;

/// Guest init script snippet for enabling Rosetta.
///
/// This should be included in the guest's init process when Rosetta is enabled.
pub fn init_script() -> &'static str {
    BINFMT_REGISTER_CMD
}

/// Platform strings that require Rosetta on ARM Macs.
pub fn needs_rosetta(platform: &str) -> bool {
    let platform_lower = platform.to_lowercase();
    platform_lower.contains("amd64")
        || platform_lower.contains("x86_64")
        || platform_lower.contains("x86-64")
}

/// Get the native platform string for this system.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn native_platform() -> &'static str {
    "linux/arm64"
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub fn native_platform() -> &'static str {
    "linux/amd64"
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub fn native_platform() -> &'static str {
    "linux/arm64"
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn native_platform() -> &'static str {
    "linux/amd64"
}

#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "macos", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
pub fn native_platform() -> &'static str {
    "linux/unknown"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_rosetta() {
        // Platform detection logic for cross-architecture support
        assert!(needs_rosetta("linux/amd64"));
        assert!(needs_rosetta("linux/x86_64"));
        assert!(needs_rosetta("LINUX/AMD64"));
        assert!(!needs_rosetta("linux/arm64"));
        assert!(!needs_rosetta("linux/aarch64"));
    }
}
