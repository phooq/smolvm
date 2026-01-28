//! Error types for smolvm.

use std::path::PathBuf;
use thiserror::Error;

/// Result type alias using smolvm's Error type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur in smolvm operations.
#[derive(Error, Debug)]
pub enum Error {
    // VM lifecycle errors
    /// Failed to create a VM.
    #[error("vm creation failed: {0}")]
    VmCreation(String),

    /// Failed to boot a VM.
    #[error("vm boot failed: {0}")]
    BootFailed(String),

    /// VM not found.
    #[error("vm not found: {0}")]
    VmNotFound(String),

    /// Hypervisor is not available.
    #[error("hypervisor unavailable: {0}")]
    HypervisorUnavailable(String),

    /// VM is in an invalid state for the requested operation.
    #[error("invalid vm state: expected {expected}, got {actual}")]
    InvalidState {
        /// Expected state.
        expected: String,
        /// Actual state.
        actual: String,
    },

    // Rootfs errors
    /// Generic rootfs error.
    #[error("rootfs error: {0}")]
    Rootfs(String),

    /// Rootfs path does not exist.
    #[error("rootfs not found: {}", path.display())]
    RootfsNotFound {
        /// Path that was not found.
        path: PathBuf,
    },

    // Storage errors
    /// Generic storage error.
    #[error("storage error: {0}")]
    Storage(String),

    /// Disk not found.
    #[error("disk not found: {}", path.display())]
    DiskNotFound {
        /// Path to the disk.
        path: PathBuf,
    },

    // Mount errors
    /// Generic mount error.
    #[error("mount error: {0}")]
    Mount(String),

    /// Invalid mount path.
    #[error("invalid mount path: {0}")]
    InvalidMountPath(String),

    /// Mount source path does not exist.
    #[error("mount source not found: {}", path.display())]
    MountSourceNotFound {
        /// Path that was not found.
        path: PathBuf,
    },

    // Configuration errors
    /// Generic configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// Failed to load configuration.
    #[error("failed to load config: {0}")]
    ConfigLoad(String),

    /// Failed to save configuration.
    #[error("failed to save config: {0}")]
    ConfigSave(String),

    /// Database error.
    #[error("database error: {0}")]
    Database(String),

    // Command execution errors
    /// External command failed.
    #[error("command failed: {command}: {message}")]
    CommandFailed {
        /// The command that failed.
        command: String,
        /// Error message.
        message: String,
    },

    // Agent VM errors
    /// Agent error.
    #[error("agent error: {0}")]
    AgentError(String),

    // KVM errors (Linux)
    /// KVM is not available (module not loaded).
    #[error("kvm unavailable: {0}")]
    KvmUnavailable(String),

    /// KVM permission denied (user not in kvm group).
    #[error("kvm permission denied: {0}")]
    KvmPermission(String),

    // IO errors
    /// IO error wrapper.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Create a rootfs error with a message.
    pub fn rootfs(msg: impl Into<String>) -> Self {
        Self::Rootfs(msg.into())
    }

    /// Create a VM creation error with a message.
    pub fn vm_creation(msg: impl Into<String>) -> Self {
        Self::VmCreation(msg.into())
    }

    /// Create a mount error with a message.
    pub fn mount(msg: impl Into<String>) -> Self {
        Self::Mount(msg.into())
    }

    /// Create a command failed error.
    pub fn command_failed(command: impl Into<String>, message: impl Into<String>) -> Self {
        Self::CommandFailed {
            command: command.into(),
            message: message.into(),
        }
    }

    /// Create a KVM unavailable error.
    pub fn kvm_unavailable(msg: impl Into<String>) -> Self {
        Self::KvmUnavailable(msg.into())
    }

    /// Create a KVM permission error.
    pub fn kvm_permission(msg: impl Into<String>) -> Self {
        Self::KvmPermission(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Error messages should include context that helps users fix the problem.
    /// These tests verify that error messages contain actionable information.

    #[test]
    fn test_vm_not_found_includes_name() {
        let err = Error::VmNotFound("my-test-vm".to_string());
        let msg = err.to_string();
        assert!(msg.contains("my-test-vm"), "Error should include VM name");
    }

    #[test]
    fn test_mount_source_not_found_includes_path() {
        let err = Error::MountSourceNotFound {
            path: PathBuf::from("/nonexistent/path"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/nonexistent/path"),
            "Error should include the path"
        );
    }

    #[test]
    fn test_rootfs_not_found_includes_path() {
        let err = Error::RootfsNotFound {
            path: PathBuf::from("/my/rootfs"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/my/rootfs"),
            "Error should include rootfs path"
        );
    }

    #[test]
    fn test_command_failed_includes_command_and_message() {
        let err = Error::command_failed("crane", "image not found");
        let msg = err.to_string();
        assert!(msg.contains("crane"), "Error should include command name");
        assert!(
            msg.contains("image not found"),
            "Error should include error message"
        );
    }

    #[test]
    fn test_invalid_state_includes_both_states() {
        let err = Error::InvalidState {
            expected: "running".to_string(),
            actual: "stopped".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("running"),
            "Error should include expected state"
        );
        assert!(msg.contains("stopped"), "Error should include actual state");
    }

    #[test]
    fn test_invalid_mount_path_includes_reason() {
        let err = Error::InvalidMountPath("source must be absolute".to_string());
        let msg = err.to_string();
        assert!(
            msg.contains("absolute"),
            "Error should explain what's wrong"
        );
    }
}
