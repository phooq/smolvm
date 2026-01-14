//! Create command implementation.

use clap::Args;
use smolvm::config::{RecordState, SmolvmConfig, VmRecord};
use smolvm::mount::{parse_mount_spec, validate_mount};
use smolvm::rootfs::buildah;
use smolvm::{HostMount, NetworkPolicy, RootfsSource, VmConfig, VmId};

/// Parse an environment variable specification (KEY=VALUE).
fn parse_env_spec(spec: &str) -> Option<(String, String)> {
    let (key, value) = spec.split_once('=')?;
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// Create a VM without starting it.
#[derive(Args, Debug)]
pub struct CreateCmd {
    /// Rootfs path or OCI image reference.
    pub source: String,

    /// Command to execute when the VM starts.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,

    /// VM name (required for create).
    #[arg(long)]
    pub name: String,

    /// Memory in MiB.
    #[arg(long, default_value = "512")]
    pub memory: u32,

    /// Number of vCPUs.
    #[arg(long, default_value = "1")]
    pub cpus: u8,

    /// Working directory inside the VM.
    #[arg(short = 'w', long)]
    pub workdir: Option<String>,

    /// Environment variable (KEY=VALUE).
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,

    /// Volume mount (host:guest[:ro]).
    #[arg(short = 'v', long = "volume")]
    pub volume: Vec<String>,

    /// Enable network egress.
    #[arg(long)]
    pub net: bool,

    /// Custom DNS server (requires --net).
    #[arg(long)]
    pub dns: Option<String>,
}

impl CreateCmd {
    /// Execute the create command.
    pub fn run(self, config: &mut SmolvmConfig) -> smolvm::Result<()> {
        // Check if VM already exists
        if config.get_vm(&self.name).is_some() {
            return Err(smolvm::Error::Config(format!(
                "VM '{}' already exists",
                self.name
            )));
        }

        // Determine rootfs source
        let (rootfs, container_id) = if std::path::Path::new(&self.source).exists() {
            tracing::info!(path = %self.source, "using path as rootfs");
            (RootfsSource::path(&self.source), None)
        } else {
            // Treat as OCI image, use buildah
            tracing::info!(image = %self.source, "pulling image via buildah");
            println!("Pulling image {}...", self.source);

            let cid = buildah::create_container(&self.source)?;
            tracing::debug!(container_id = %cid, "created buildah container");

            (RootfsSource::buildah(&cid), Some(cid))
        };

        // Parse environment variables
        let env: Vec<(String, String)> = self
            .env
            .iter()
            .filter_map(|e| parse_env_spec(e))
            .collect();

        // Parse volume mounts
        let mounts: Vec<HostMount> = self
            .volume
            .iter()
            .filter_map(|v| match parse_mount_spec(v) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(spec = %v, error = %e, "invalid mount spec, skipping");
                    eprintln!("Warning: invalid mount spec '{}': {}", v, e);
                    None
                }
            })
            .collect();

        // Validate each mount
        for mount in &mounts {
            validate_mount(mount)?;
        }

        // Build network policy
        let network = if self.net {
            let dns = self.dns.as_ref().and_then(|d| d.parse().ok());
            NetworkPolicy::Egress { dns }
        } else {
            NetworkPolicy::None
        };

        // Build VM config
        let mut builder = VmConfig::builder(rootfs)
            .id(VmId::new(&self.name))
            .memory(self.memory)
            .cpus(self.cpus)
            .network(network);

        // Set command
        if !self.command.is_empty() {
            builder = builder.command(self.command.clone());
        }

        // Set working directory
        if let Some(wd) = &self.workdir {
            builder = builder.workdir(wd);
        }

        // Add environment variables
        for (k, v) in env {
            builder = builder.env(k, v);
        }

        // Add mounts
        for m in mounts {
            builder = builder.mount(m);
        }

        let vm_config = builder.build();

        // Create record with Created state
        let mut record = VmRecord::from_config(&vm_config);
        record.state = RecordState::Created;

        // Store in config
        config.vms.insert(self.name.clone(), record);
        config.save()?;

        println!("Created VM: {}", self.name);
        if let Some(cid) = container_id {
            tracing::debug!(container_id = %cid, "buildah container preserved");
        }

        Ok(())
    }
}
