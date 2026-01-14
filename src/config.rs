//! Global smolvm configuration.
//!
//! This module handles persistent configuration storage for smolvm,
//! including default settings and VM registry.

use crate::error::{Error, Result};
use crate::vm::config::VmConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// VM lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RecordState {
    /// Container exists, VM not started.
    #[default]
    Created,
    /// VM process is running.
    Running,
    /// VM exited cleanly.
    Stopped,
    /// VM crashed or error.
    Failed,
}

impl std::fmt::Display for RecordState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordState::Created => write!(f, "created"),
            RecordState::Running => write!(f, "running"),
            RecordState::Stopped => write!(f, "stopped"),
            RecordState::Failed => write!(f, "failed"),
        }
    }
}

/// Application name for config file storage.
const APP_NAME: &str = "smolvm";

/// Global smolvm configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmolvmConfig {
    /// Configuration format version.
    pub version: u8,

    /// Default number of vCPUs for new VMs.
    pub default_cpus: u8,

    /// Default memory in MiB for new VMs.
    pub default_mem: u32,

    /// Default DNS server for VMs with network egress.
    pub default_dns: String,

    /// Storage volume path (macOS only, for case-sensitive filesystem).
    #[cfg(target_os = "macos")]
    #[serde(default)]
    pub storage_volume: String,

    /// Registry of known VMs (by name).
    #[serde(default)]
    pub vms: HashMap<String, VmRecord>,
}

impl Default for SmolvmConfig {
    fn default() -> Self {
        Self {
            version: 1,
            default_cpus: 1,
            default_mem: 512,
            default_dns: "1.1.1.1".to_string(),
            #[cfg(target_os = "macos")]
            storage_volume: String::new(),
            vms: HashMap::new(),
        }
    }
}

impl SmolvmConfig {
    /// Load configuration from disk.
    ///
    /// If the configuration file doesn't exist, returns the default configuration.
    pub fn load() -> Result<Self> {
        confy::load(APP_NAME, None).map_err(|e| Error::ConfigLoad(e.to_string()))
    }

    /// Save configuration to disk.
    pub fn save(&self) -> Result<()> {
        confy::store(APP_NAME, None, self).map_err(|e| Error::ConfigSave(e.to_string()))
    }

    /// Add a VM to the registry.
    pub fn add_vm(&mut self, config: &VmConfig) {
        let record = VmRecord::from_config(config);
        self.vms.insert(config.id.0.clone(), record);
    }

    /// Remove a VM from the registry.
    pub fn remove_vm(&mut self, id: &str) -> Option<VmRecord> {
        self.vms.remove(id)
    }

    /// Get a VM record by ID.
    pub fn get_vm(&self, id: &str) -> Option<&VmRecord> {
        self.vms.get(id)
    }

    /// List all VM records.
    pub fn list_vms(&self) -> impl Iterator<Item = (&String, &VmRecord)> {
        self.vms.iter()
    }

    /// Update a VM record in place.
    pub fn update_vm<F>(&mut self, id: &str, f: F) -> Option<()>
    where
        F: FnOnce(&mut VmRecord),
    {
        if let Some(record) = self.vms.get_mut(id) {
            f(record);
            Some(())
        } else {
            None
        }
    }
}

/// Record of a VM in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmRecord {
    /// VM name/ID.
    pub name: String,

    /// Number of vCPUs.
    pub cpus: u8,

    /// Memory in MiB.
    pub mem: u32,

    /// Buildah container ID (if using buildah rootfs).
    pub container_id: Option<String>,

    /// Original rootfs source description.
    pub rootfs_source: String,

    /// Creation timestamp.
    pub created_at: String,

    /// VM lifecycle state.
    #[serde(default)]
    pub state: RecordState,

    /// Process ID when running.
    #[serde(default)]
    pub pid: Option<i32>,

    /// Path to PID file.
    #[serde(default)]
    pub pid_file: Option<String>,

    /// Command to execute.
    #[serde(default)]
    pub command: Option<Vec<String>>,

    /// Working directory.
    #[serde(default)]
    pub workdir: Option<String>,

    /// Environment variables.
    #[serde(default)]
    pub env: Vec<(String, String)>,

    /// Network enabled.
    #[serde(default)]
    pub net_enabled: bool,

    /// DNS server.
    #[serde(default)]
    pub dns: Option<String>,
}

impl VmRecord {
    /// Create a record from a VM configuration.
    pub fn from_config(config: &VmConfig) -> Self {
        use crate::vm::config::{NetworkPolicy, RootfsSource};

        let (container_id, rootfs_source) = match &config.rootfs {
            RootfsSource::Path { path } => (None, path.display().to_string()),
            RootfsSource::Buildah { container_id } => {
                (Some(container_id.clone()), format!("buildah:{}", container_id))
            }
        };

        let (net_enabled, dns) = match &config.network {
            NetworkPolicy::None => (false, None),
            NetworkPolicy::Egress { dns } => (true, dns.map(|ip| ip.to_string())),
        };

        Self {
            name: config.id.0.clone(),
            cpus: config.resources.cpus,
            mem: config.resources.memory_mib,
            container_id,
            rootfs_source,
            created_at: chrono_lite_now(),
            state: RecordState::Created,
            pid: None,
            pid_file: None,
            command: config.command.clone(),
            workdir: config.workdir.as_ref().map(|p| p.to_string_lossy().to_string()),
            env: config.env.clone(),
            net_enabled,
            dns,
        }
    }

    /// Check if the VM process is still alive.
    pub fn is_process_alive(&self) -> bool {
        if let Some(pid) = self.pid {
            // Check if process exists by sending signal 0
            unsafe { libc::kill(pid, 0) == 0 }
        } else {
            false
        }
    }

    /// Get the actual state, checking if running process is still alive.
    pub fn actual_state(&self) -> RecordState {
        if self.state == RecordState::Running {
            if self.is_process_alive() {
                RecordState::Running
            } else {
                RecordState::Stopped // Process died
            }
        } else {
            self.state.clone()
        }
    }
}

/// Get current timestamp as ISO 8601 string (simplified, no chrono dependency).
fn chrono_lite_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    // Simple ISO-like format
    format!("{}", duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::config::{Resources, RootfsSource, VmId};

    #[test]
    fn test_add_and_get_vm() {
        let mut config = SmolvmConfig::default();

        let vm_config = VmConfig {
            id: VmId::new("test-vm"),
            rootfs: RootfsSource::path("/some/path"),
            resources: Resources::new(1024, 2),
            timeouts: Default::default(),
            network: Default::default(),
            mounts: vec![],
            disks: vec![],
            vsock_ports: vec![],
            console_log: None,
            rosetta: false,
            command: None,
            workdir: None,
            env: vec![],
        };

        config.add_vm(&vm_config);

        let record = config.get_vm("test-vm").expect("VM should exist");
        assert_eq!(record.name, "test-vm");
        assert_eq!(record.cpus, 2);
        assert_eq!(record.mem, 1024);
    }

    #[test]
    fn test_remove_vm() {
        let mut config = SmolvmConfig::default();

        let vm_config = VmConfig {
            id: VmId::new("test-vm"),
            rootfs: RootfsSource::path("/some/path"),
            resources: Resources::default(),
            timeouts: Default::default(),
            network: Default::default(),
            mounts: vec![],
            disks: vec![],
            vsock_ports: vec![],
            console_log: None,
            rosetta: false,
            command: None,
            workdir: None,
            env: vec![],
        };

        config.add_vm(&vm_config);
        assert!(config.get_vm("test-vm").is_some());

        let removed = config.remove_vm("test-vm");
        assert!(removed.is_some());
        assert!(config.get_vm("test-vm").is_none());
    }

    #[test]
    fn test_list_vms() {
        let mut config = SmolvmConfig::default();

        for i in 0..3 {
            let vm_config = VmConfig {
                id: VmId::new(format!("vm-{}", i)),
                rootfs: RootfsSource::path("/some/path"),
                resources: Resources::default(),
                timeouts: Default::default(),
                network: Default::default(),
                mounts: vec![],
                disks: vec![],
                vsock_ports: vec![],
                console_log: None,
                rosetta: false,
                command: None,
                workdir: None,
                env: vec![],
            };
            config.add_vm(&vm_config);
        }

        let vms: Vec<_> = config.list_vms().collect();
        assert_eq!(vms.len(), 3);
    }

    #[test]
    fn test_vm_record_serialization() {
        let record = VmRecord {
            name: "test".to_string(),
            cpus: 2,
            mem: 1024,
            container_id: Some("abc123".to_string()),
            rootfs_source: "buildah:abc123".to_string(),
            created_at: "1234567890".to_string(),
            state: RecordState::Created,
            pid: None,
            pid_file: None,
            command: Some(vec!["/bin/echo".to_string(), "hello".to_string()]),
            workdir: Some("/app".to_string()),
            env: vec![("FOO".to_string(), "bar".to_string())],
            net_enabled: true,
            dns: Some("8.8.8.8".to_string()),
        };

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: VmRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.name, record.name);
        assert_eq!(deserialized.cpus, record.cpus);
        assert_eq!(deserialized.container_id, record.container_id);
        assert_eq!(deserialized.command, record.command);
        assert_eq!(deserialized.env, record.env);
        assert_eq!(deserialized.net_enabled, record.net_enabled);
    }

    // === Config Migration / Backwards Compatibility ===

    #[test]
    fn test_config_v1_backwards_compat() {
        // Ensure we can deserialize a v1 config format
        let v1_json = r#"{
            "version": 1,
            "default_cpus": 2,
            "default_mem": 1024,
            "default_dns": "8.8.8.8",
            "vms": {}
        }"#;

        let config: SmolvmConfig = serde_json::from_str(v1_json).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.default_cpus, 2);
        assert_eq!(config.default_mem, 1024);
        assert_eq!(config.default_dns, "8.8.8.8");
    }

    #[test]
    fn test_config_with_existing_vms() {
        // Ensure we can deserialize config with VM records
        let json = r#"{
            "version": 1,
            "default_cpus": 1,
            "default_mem": 512,
            "default_dns": "1.1.1.1",
            "vms": {
                "my-vm": {
                    "name": "my-vm",
                    "cpus": 4,
                    "mem": 2048,
                    "container_id": "abc123",
                    "rootfs_source": "alpine:latest",
                    "created_at": "1234567890"
                }
            }
        }"#;

        let config: SmolvmConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.vms.len(), 1);

        let vm = config.get_vm("my-vm").unwrap();
        assert_eq!(vm.cpus, 4);
        assert_eq!(vm.mem, 2048);
        assert_eq!(vm.container_id, Some("abc123".to_string()));
    }

    #[test]
    fn test_config_missing_optional_fields() {
        // Config should handle missing optional fields via defaults
        let minimal_json = r#"{
            "version": 1,
            "default_cpus": 1,
            "default_mem": 512,
            "default_dns": "1.1.1.1"
        }"#;

        let config: SmolvmConfig = serde_json::from_str(minimal_json).unwrap();
        assert!(config.vms.is_empty()); // vms has #[serde(default)]
    }

    #[test]
    fn test_vm_record_without_container_id() {
        // VM record with null container_id (path-based rootfs)
        let json = r#"{
            "name": "local-vm",
            "cpus": 1,
            "mem": 512,
            "container_id": null,
            "rootfs_source": "/path/to/rootfs",
            "created_at": "1234567890"
        }"#;

        let record: VmRecord = serde_json::from_str(json).unwrap();
        assert!(record.container_id.is_none());
        assert_eq!(record.rootfs_source, "/path/to/rootfs");
    }
}
