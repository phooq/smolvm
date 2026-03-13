//! Shared VM inventory and lifecycle service.
//!
//! This module provides a reusable abstraction over the shared redb-backed VM
//! inventory and the `AgentManager` runtime lifecycle. It is intended to be the
//! common backend for CLI, API, and language bindings.

use crate::agent::{vm_data_dir, AgentClient, AgentManager};
use crate::config::{RecordState, VmRecord, DEFAULT_VM_CPUS, DEFAULT_VM_MEMORY_MIB};
use crate::{Error, Result, SmolvmDb};
use std::fs;
use std::path::Path;

/// Maximum supported VM name length.
const MAX_NAME_LENGTH: usize = 40;

/// Targets a named VM or the shared default VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmTarget {
    /// The shared default VM.
    Default,
    /// A specifically named VM.
    Named(String),
}

impl VmTarget {
    /// Return the persisted VM name for this target.
    pub fn name(&self) -> &str {
        match self {
            Self::Default => "default",
            Self::Named(name) => name,
        }
    }
}

/// Durable VM configuration used by the service create/ensure APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSpec {
    /// Unique VM name.
    pub name: String,
    /// Number of vCPUs.
    pub cpus: u8,
    /// Memory in MiB.
    pub mem: u32,
    /// Host mounts as `(source, target, read_only)`.
    pub mounts: Vec<(String, String, bool)>,
    /// Port mappings as `(host, guest)`.
    pub ports: Vec<(u16, u16)>,
    /// Whether outbound network access is enabled.
    pub network: bool,
    /// Init commands run after a successful VM start.
    pub init: Vec<String>,
    /// Environment variables for init commands.
    pub env: Vec<(String, String)>,
    /// Working directory for init commands.
    pub workdir: Option<String>,
    /// Storage disk size override in GiB.
    pub storage_gb: Option<u64>,
    /// Overlay disk size override in GiB.
    pub overlay_gb: Option<u64>,
}

impl VmSpec {
    /// Create a new VM spec with the required fields.
    pub fn new(
        name: String,
        cpus: u8,
        mem: u32,
        mounts: Vec<(String, String, bool)>,
        ports: Vec<(u16, u16)>,
        network: bool,
    ) -> Self {
        Self {
            name,
            cpus,
            mem,
            mounts,
            ports,
            network,
            init: Vec::new(),
            env: Vec::new(),
            workdir: None,
            storage_gb: None,
            overlay_gb: None,
        }
    }
}

/// Shared VM inventory view derived from a persisted record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmSummary {
    /// VM name.
    pub name: String,
    /// Effective lifecycle state.
    pub state: RecordState,
    /// Running PID when present.
    pub pid: Option<i32>,
    /// Verified process start time when present.
    pub pid_start_time: Option<u64>,
    /// Number of vCPUs.
    pub cpus: u8,
    /// Memory in MiB.
    pub mem: u32,
    /// Host mounts as `(source, target, read_only)`.
    pub mounts: Vec<(String, String, bool)>,
    /// Port mappings as `(host, guest)`.
    pub ports: Vec<(u16, u16)>,
    /// Whether outbound network access is enabled.
    pub network: bool,
    /// Creation timestamp.
    pub created_at: String,
    /// Storage disk size override in GiB.
    pub storage_gb: Option<u64>,
    /// Overlay disk size override in GiB.
    pub overlay_gb: Option<u64>,
}

/// Result of starting a VM through the service.
pub struct VmStartResult {
    /// Live agent manager for the started or reused VM.
    pub manager: AgentManager,
    /// Updated persisted record after start-state persistence.
    pub record: VmRecord,
    /// Whether the VM was already running and reused.
    pub already_running: bool,
    /// Current child PID when available.
    pub pid: Option<i32>,
    /// Verified process start time when available.
    pub pid_start_time: Option<u64>,
}

/// Shared VM inventory and lifecycle service.
#[derive(Debug, Clone)]
pub struct VmService {
    db: SmolvmDb,
}

impl VmService {
    /// Construct the service using the default smolvm database path.
    pub fn new() -> Result<Self> {
        Self::from_db_path(SmolvmDb::default_path()?)
    }

    /// Construct the service using an explicit database path.
    pub fn from_db_path(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            db: SmolvmDb::open_at(path.as_ref())?,
        })
    }

    /// Construct the service with a specific database handle.
    pub fn with_db(db: SmolvmDb) -> Self {
        Self { db }
    }

    /// Resolve an optional name into a concrete VM target.
    pub fn resolve_target(name: Option<&str>) -> VmTarget {
        match name {
            None | Some("default") => VmTarget::Default,
            Some(name) => VmTarget::Named(name.to_string()),
        }
    }

    /// Validate a VM name using the shared naming rules.
    pub fn validate_name(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(Error::config(
                "validate vm name",
                "vm name cannot be empty",
            ));
        }
        if name.len() > MAX_NAME_LENGTH {
            return Err(Error::config(
                "validate vm name",
                format!(
                    "vm name too long: {} characters (max {})",
                    name.len(),
                    MAX_NAME_LENGTH
                ),
            ));
        }

        let first = name.chars().next().unwrap_or_default();
        if !first.is_ascii_alphanumeric() {
            return Err(Error::config(
                "validate vm name",
                "vm name must start with a letter or digit",
            ));
        }

        if name.ends_with('-') {
            return Err(Error::config(
                "validate vm name",
                "vm name cannot end with a hyphen",
            ));
        }

        let mut prev_was_hyphen = false;
        for ch in name.chars() {
            if ch == '-' {
                if prev_was_hyphen {
                    return Err(Error::config(
                        "validate vm name",
                        "vm name cannot contain consecutive hyphens",
                    ));
                }
                prev_was_hyphen = true;
                continue;
            }

            prev_was_hyphen = false;
            if !ch.is_ascii_alphanumeric() && ch != '_' {
                return Err(Error::config(
                    "validate vm name",
                    format!("vm name contains invalid character: '{}'", ch),
                ));
            }
        }

        Ok(())
    }

    /// Get a persisted VM record by target.
    pub fn get_record(&self, target: &VmTarget) -> Result<Option<VmRecord>> {
        self.db.get_vm(target.name())
    }

    /// List all persisted VM records.
    pub fn list_records(&self) -> Result<Vec<(String, VmRecord)>> {
        let mut records = self.db.list_vms()?;
        records.sort_by(|lhs, rhs| lhs.0.cmp(&rhs.0));
        Ok(records)
    }

    /// Get a summary view for a single VM target.
    pub fn get_summary(&self, target: &VmTarget) -> Result<Option<VmSummary>> {
        Ok(self
            .get_record(target)?
            .map(|record| summary_from_record(target.name(), &record)))
    }

    /// List summary views for all persisted VMs.
    pub fn list_summaries(&self) -> Result<Vec<VmSummary>> {
        self.list_records()?
            .into_iter()
            .map(|(name, record)| Ok(summary_from_record(&name, &record)))
            .collect()
    }

    /// Create a new persisted VM record.
    pub fn create_vm(&self, spec: VmSpec) -> Result<VmRecord> {
        Self::validate_name(&spec.name)?;
        let name = spec.name.clone();
        let record = record_from_spec(spec);
        match self.db.insert_vm_if_not_exists(&name, &record)? {
            true => Ok(record),
            false => Err(Error::config(
                "create vm",
                format!("vm '{}' already exists", name),
            )),
        }
    }

    /// Ensure a VM record exists and return the resulting record.
    pub fn ensure_vm(&self, spec: VmSpec) -> Result<VmRecord> {
        Self::validate_name(&spec.name)?;
        let name = spec.name.clone();
        if let Some(record) = self.db.get_vm(&name)? {
            return Ok(record);
        }

        let record = record_from_spec(spec);
        let _inserted = self.db.insert_vm_if_not_exists(&name, &record)?;
        self.db
            .get_vm(&name)?
            .ok_or_else(|| Error::database("ensure vm", format!("vm '{}' missing after insert", name)))
    }

    /// Update a VM record to the running state.
    pub fn update_running_state(&self, name: &str, pid: Option<i32>) -> Result<Option<VmRecord>> {
        let pid_start_time = pid.and_then(crate::process::process_start_time);
        self.db.update_vm(name, |record| {
            record.state = RecordState::Running;
            record.pid = pid;
            record.pid_start_time = pid_start_time;
        })
    }

    /// Update a VM record to the stopped state.
    pub fn update_stopped_state(&self, name: &str) -> Result<Option<VmRecord>> {
        self.db.update_vm(name, |record| {
            record.state = RecordState::Stopped;
            record.pid = None;
            record.pid_start_time = None;
        })
    }

    /// Remove a VM record from persistence.
    pub fn delete_vm_record(&self, name: &str) -> Result<Option<VmRecord>> {
        self.db.remove_vm(name)
    }

    /// Create an agent manager for a target, optionally using record-backed disk sizes.
    pub fn manager_for_target(
        &self,
        target: &VmTarget,
        record: Option<&VmRecord>,
    ) -> Result<AgentManager> {
        match target {
            VmTarget::Default => match record {
                Some(record) => {
                    AgentManager::new_default_with_sizes(record.storage_gb, record.overlay_gb)
                }
                None => AgentManager::new_default(),
            },
            VmTarget::Named(name) => match record {
                Some(record) => {
                    AgentManager::for_vm_with_sizes(name, record.storage_gb, record.overlay_gb)
                }
                None => AgentManager::for_vm(name),
            },
        }
    }

    /// Connect to an already-running VM.
    pub fn connect_vm(&self, target: &VmTarget) -> Result<(AgentManager, AgentClient)> {
        let record = self.get_record(target)?;
        let manager = self.manager_for_target(target, record.as_ref())?;

        if manager.try_connect_existing().is_none() {
            return Err(Error::agent(
                "connect",
                format!("vm '{}' is not running", target.name()),
            ));
        }

        let client = manager.connect()?;
        Ok((manager, client))
    }

    /// Start a VM from persisted configuration and update its shared state.
    pub fn start_vm(&self, target: &VmTarget) -> Result<VmStartResult> {
        let mut record = match self.get_record(target)? {
            Some(record) => record,
            None if matches!(target, VmTarget::Default) => self.ensure_vm(default_vm_spec())?,
            None => return Err(Error::vm_not_found(target.name())),
        };

        let manager = self.manager_for_target(target, Some(&record))?;
        let started = manager.ensure_running_with_full_config(
            record.host_mounts(),
            record.port_mappings(),
            record.vm_resources(),
        )?;

        if started && !record.init.is_empty() {
            run_init_commands(&record, &manager)?;
        }

        let pid = manager.child_pid();
        let pid_start_time = pid.and_then(crate::process::process_start_time);

        if let Some(updated) = self.update_running_state(target.name(), pid)? {
            record = updated;
        }

        Ok(VmStartResult {
            manager,
            record,
            already_running: !started,
            pid,
            pid_start_time,
        })
    }

    /// Stop a running VM and best-effort persist the stopped state.
    pub fn stop_vm(&self, target: &VmTarget) -> Result<()> {
        let record = self.get_record(target)?;
        let manager = self.manager_for_target(target, record.as_ref())?;

        let has_runtime = manager.try_connect_existing().is_some();
        let has_record = record.is_some();
        if !has_runtime && !has_record {
            return Err(Error::vm_not_found(target.name()));
        }

        manager.stop()?;
        let _ = self.update_stopped_state(target.name())?;
        Ok(())
    }

    /// Delete a VM record and its data directory, optionally stopping it first.
    pub fn delete_vm(&self, target: &VmTarget, stop_if_running: bool) -> Result<()> {
        let name = target.name().to_string();
        let record = self.get_record(target)?;

        if stop_if_running && record.as_ref().is_some_and(|record| record.actual_state() == RecordState::Running) {
            let _ = self.stop_vm(target);
        }

        let removed = self.delete_vm_record(&name)?;
        let data_dir = vm_data_dir(&name);
        let had_data_dir = data_dir.exists();
        if had_data_dir {
            fs::remove_dir_all(&data_dir)
                .map_err(|e| Error::storage("remove vm data directory", e.to_string()))?;
        }

        if removed.is_none() && !had_data_dir {
            return Err(Error::vm_not_found(name));
        }

        Ok(())
    }
}

fn record_from_spec(spec: VmSpec) -> VmRecord {
    let mut record = VmRecord::new(
        spec.name,
        spec.cpus,
        spec.mem,
        spec.mounts,
        spec.ports,
        spec.network,
    );
    record.init = spec.init;
    record.env = spec.env;
    record.workdir = spec.workdir;
    record.storage_gb = spec.storage_gb;
    record.overlay_gb = spec.overlay_gb;
    record
}

fn default_vm_spec() -> VmSpec {
    VmSpec::new(
        "default".to_string(),
        DEFAULT_VM_CPUS,
        DEFAULT_VM_MEMORY_MIB,
        Vec::new(),
        Vec::new(),
        false,
    )
}

fn summary_from_record(name: &str, record: &VmRecord) -> VmSummary {
    let state = record.actual_state();
    let pid = if state == RecordState::Stopped {
        None
    } else {
        record.pid
    };
    let pid_start_time = if state == RecordState::Stopped {
        None
    } else {
        record.pid_start_time
    };

    VmSummary {
        name: name.to_string(),
        state,
        pid,
        pid_start_time,
        cpus: record.cpus,
        mem: record.mem,
        mounts: record.mounts.clone(),
        ports: record.ports.clone(),
        network: record.network,
        created_at: record.created_at.clone(),
        storage_gb: record.storage_gb,
        overlay_gb: record.overlay_gb,
    }
}

fn run_init_commands(record: &VmRecord, manager: &AgentManager) -> Result<()> {
    let mut client = AgentClient::connect_with_retry(manager.vsock_socket())?;
    for (index, cmd) in record.init.iter().enumerate() {
        let argv = vec!["sh".to_string(), "-c".to_string(), cmd.clone()];
        let (exit_code, _stdout, stderr) =
            client.vm_exec(argv, record.env.clone(), record.workdir.clone(), None)?;
        if exit_code != 0 {
            tracing::warn!(
                vm = %record.name,
                index,
                exit_code,
                stderr = stderr.trim(),
                "vm init command failed"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_service() -> (tempfile::TempDir, VmService) {
        let dir = tempfile::tempdir().unwrap();
        let db = SmolvmDb::open_at(&dir.path().join("test.redb")).unwrap();
        (dir, VmService::with_db(db))
    }

    #[test]
    fn create_vm_inserts_record() {
        let (_dir, service) = temp_service();
        let record = service
            .create_vm(VmSpec::new(
                "demo".to_string(),
                2,
                1024,
                vec![("/host".to_string(), "/guest".to_string(), true)],
                vec![(8080, 80)],
                true,
            ))
            .unwrap();

        assert_eq!(record.name, "demo");
        assert_eq!(service.get_record(&VmTarget::Named("demo".to_string())).unwrap().unwrap().name, "demo");
    }

    #[test]
    fn create_vm_rejects_duplicate_name() {
        let (_dir, service) = temp_service();
        let spec = VmSpec::new("demo".to_string(), 1, 512, vec![], vec![], false);
        service.create_vm(spec.clone()).unwrap();

        let err = service.create_vm(spec).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn ensure_vm_inserts_when_missing() {
        let (_dir, service) = temp_service();
        let record = service
            .ensure_vm(VmSpec::new("demo".to_string(), 1, 512, vec![], vec![], false))
            .unwrap();

        assert_eq!(record.name, "demo");
        assert!(service.get_record(&VmTarget::Named("demo".to_string())).unwrap().is_some());
    }

    #[test]
    fn ensure_vm_returns_existing_when_present() {
        let (_dir, service) = temp_service();
        let mut spec = VmSpec::new("demo".to_string(), 1, 512, vec![], vec![], false);
        spec.init.push("echo first".to_string());
        service.create_vm(spec).unwrap();

        let record = service
            .ensure_vm(VmSpec::new("demo".to_string(), 4, 4096, vec![], vec![], true))
            .unwrap();

        assert_eq!(record.cpus, 1);
        assert_eq!(record.init, vec!["echo first".to_string()]);
    }

    #[test]
    fn get_summary_clears_pid_when_actual_state_is_stopped() {
        let (_dir, service) = temp_service();
        let current_pid = std::process::id() as i32;
        let mut record = service
            .create_vm(VmSpec::new("demo".to_string(), 1, 512, vec![], vec![], false))
            .unwrap();
        record.state = RecordState::Running;
        record.pid = Some(current_pid + 100_000);
        record.pid_start_time = Some(42);
        service.db.insert_vm("demo", &record).unwrap();

        let summary = service
            .get_summary(&VmTarget::Named("demo".to_string()))
            .unwrap()
            .unwrap();

        assert_eq!(summary.state, RecordState::Stopped);
        assert_eq!(summary.pid, None);
        assert_eq!(summary.pid_start_time, None);
    }

    #[test]
    fn list_summaries_returns_all_records() {
        let (_dir, service) = temp_service();
        service
            .create_vm(VmSpec::new("b".to_string(), 1, 512, vec![], vec![], false))
            .unwrap();
        service
            .create_vm(VmSpec::new("a".to_string(), 2, 1024, vec![], vec![], true))
            .unwrap();

        let summaries = service.list_summaries().unwrap();
        let names: Vec<_> = summaries.into_iter().map(|summary| summary.name).collect();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn update_running_state_sets_pid_and_start_time() {
        let (_dir, service) = temp_service();
        service
            .create_vm(VmSpec::new("demo".to_string(), 1, 512, vec![], vec![], false))
            .unwrap();

        let pid = std::process::id() as i32;
        let updated = service.update_running_state("demo", Some(pid)).unwrap().unwrap();

        assert_eq!(updated.state, RecordState::Running);
        assert_eq!(updated.pid, Some(pid));
        assert_eq!(updated.pid_start_time, crate::process::process_start_time(pid));
    }

    #[test]
    fn update_stopped_state_clears_pid_fields() {
        let (_dir, service) = temp_service();
        let mut record = service
            .create_vm(VmSpec::new("demo".to_string(), 1, 512, vec![], vec![], false))
            .unwrap();
        record.state = RecordState::Running;
        record.pid = Some(std::process::id() as i32);
        record.pid_start_time = Some(123);
        service.db.insert_vm("demo", &record).unwrap();

        let updated = service.update_stopped_state("demo").unwrap().unwrap();
        assert_eq!(updated.state, RecordState::Stopped);
        assert_eq!(updated.pid, None);
        assert_eq!(updated.pid_start_time, None);
    }

    #[test]
    fn delete_vm_record_removes_row() {
        let (_dir, service) = temp_service();
        service
            .create_vm(VmSpec::new("demo".to_string(), 1, 512, vec![], vec![], false))
            .unwrap();

        let removed = service.delete_vm_record("demo").unwrap();
        assert!(removed.is_some());
        assert!(service.get_record(&VmTarget::Named("demo".to_string())).unwrap().is_none());
    }

    #[test]
    fn resolve_target_maps_default_and_named_targets() {
        assert_eq!(VmService::resolve_target(None), VmTarget::Default);
        assert_eq!(VmService::resolve_target(Some("default")), VmTarget::Default);
        assert_eq!(
            VmService::resolve_target(Some("named")),
            VmTarget::Named("named".to_string())
        );
    }

    #[test]
    fn start_vm_requires_named_record_to_exist() {
        let (_dir, service) = temp_service();
        match service.start_vm(&VmTarget::Named("missing".to_string())) {
            Ok(_) => panic!("expected missing VM start to fail"),
            Err(err) => assert!(matches!(err, Error::VmNotFound { .. })),
        }
    }

    #[test]
    fn from_db_path_creates_service_at_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("service.redb");

        let service = VmService::from_db_path(&path).unwrap();
        let _record = service
            .create_vm(VmSpec::new("demo".to_string(), 1, 512, vec![], vec![], false))
            .unwrap();

        assert!(path.parent().unwrap().exists());
        assert!(path.exists());
    }
}
