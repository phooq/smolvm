//! Helper VM management.
//!
//! This module manages the helper VM lifecycle and provides a client
//! for communicating with the helper daemon via vsock.

mod client;
mod launcher;
mod manager;

pub use client::HelperClient;
pub use manager::{HelperManager, HelperState};

/// Default helper VM memory in MiB.
pub const HELPER_MEMORY_MIB: u32 = 256;

/// Default helper VM CPU count.
pub const HELPER_CPUS: u8 = 1;

/// Helper VM name.
pub const HELPER_VM_NAME: &str = "smolvm-helper";
