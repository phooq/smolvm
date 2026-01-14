//! Run command implementation.

use clap::Args;
use smolvm::config::SmolvmConfig;
use smolvm::mount::{parse_mount_spec, validate_mount};
use smolvm::rootfs::buildah;
use smolvm::{default_backend, HostMount, NetworkPolicy, RootfsSource, VmConfig, VmId};

/// Parse an environment variable specification (KEY=VALUE).
///
/// Returns None if the spec is invalid (no '=' present).
fn parse_env_spec(spec: &str) -> Option<(String, String)> {
    let (key, value) = spec.split_once('=')?;
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// Run a VM from a rootfs path or OCI image.
#[derive(Args, Debug)]
pub struct RunCmd {
    /// Rootfs path or OCI image reference.
    ///
    /// If the path exists on disk, it's used directly as the rootfs.
    /// Otherwise, it's treated as an OCI image reference and pulled via buildah.
    pub source: String,

    /// Command to execute inside the VM.
    ///
    /// If not specified, defaults to /bin/sh.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,

    /// VM name (auto-generated if not provided).
    #[arg(long)]
    pub name: Option<String>,

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
    ///
    /// Can be specified multiple times.
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,

    /// Volume mount (host:guest[:ro]).
    ///
    /// Can be specified multiple times.
    /// Example: -v /host/path:/guest/path
    #[arg(short = 'v', long = "volume")]
    pub volume: Vec<String>,

    /// Enable network egress.
    ///
    /// Allows the VM to access the internet via NAT.
    #[arg(long)]
    pub net: bool,

    /// Custom DNS server (requires --net).
    #[arg(long)]
    pub dns: Option<String>,

    /// Write console output to file (for debugging).
    #[arg(long)]
    pub console_log: Option<String>,
}

impl RunCmd {
    /// Execute the run command.
    pub fn run(self, config: &mut SmolvmConfig) -> smolvm::Result<()> {
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

        // Validate each mount (source exists, paths are absolute)
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
            .memory(self.memory)
            .cpus(self.cpus)
            .network(network);

        // Set VM ID
        if let Some(name) = &self.name {
            builder = builder.id(VmId::new(name));
        }

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

        // Set console log if specified
        if let Some(ref log_path) = self.console_log {
            builder = builder.console_log(log_path);
        }

        let vm_config = builder.build();
        let vm_id = vm_config.id.clone();

        // Store VM in config
        config.add_vm(&vm_config);

        // Create and run VM - use a closure to ensure cleanup on all paths
        let result = self.run_vm(vm_config, &vm_id);

        // Cleanup buildah container if we created one (runs on success AND failure)
        if let Some(cid) = container_id {
            tracing::debug!(container_id = %cid, "cleaning up buildah container");
            if let Err(e) = buildah::remove_container(&cid) {
                tracing::warn!(error = %e, "failed to remove buildah container");
            }
            config.remove_vm(vm_id.as_str());
        }

        // Save config before exiting (process::exit bypasses main's save)
        if let Err(e) = config.save() {
            tracing::warn!(error = %e, "failed to save config");
        }

        // Handle result
        match result {
            Ok(exit_code) => std::process::exit(exit_code),
            Err(e) => {
                tracing::error!(error = %e, "VM execution failed");
                return Err(e);
            }
        }
    }

    /// Run the VM and return exit code. Separated to allow cleanup on all paths.
    fn run_vm(&self, vm_config: VmConfig, vm_id: &VmId) -> smolvm::Result<i32> {
        println!("Starting VM {}...", vm_id);
        tracing::info!(vm_id = %vm_id, cpus = %vm_config.resources.cpus, memory = %vm_config.resources.memory_mib, "starting VM");

        let backend = default_backend()?;
        tracing::debug!(backend = %backend.name(), "using backend");

        let mut vm = backend.create(vm_config)?;

        let exit = vm.wait()?;
        tracing::info!(vm_id = %vm_id, exit = ?exit, "VM exited");

        // Print console output if captured to a log file
        if let Some(ref log_path) = self.console_log {
            if let Ok(contents) = std::fs::read_to_string(log_path) {
                if !contents.is_empty() {
                    print!("{}", contents);
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
            }
        }

        Ok(exit.exit_code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // Wrapper for testing CLI parsing
    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        run: RunCmd,
    }

    fn parse_run(args: &[&str]) -> RunCmd {
        let mut full_args = vec!["smolvm"];
        full_args.extend(args);
        TestCli::parse_from(full_args).run
    }

    // === Environment Variable Parsing ===

    #[test]
    fn test_parse_env_spec_valid() {
        assert_eq!(
            parse_env_spec("FOO=bar"),
            Some(("FOO".to_string(), "bar".to_string()))
        );
    }

    #[test]
    fn test_parse_env_spec_empty_value() {
        // Empty value is valid (FOO=)
        assert_eq!(
            parse_env_spec("FOO="),
            Some(("FOO".to_string(), "".to_string()))
        );
    }

    #[test]
    fn test_parse_env_spec_value_with_equals() {
        // Value containing = should work (FOO=bar=baz)
        assert_eq!(
            parse_env_spec("FOO=bar=baz"),
            Some(("FOO".to_string(), "bar=baz".to_string()))
        );
    }

    #[test]
    fn test_parse_env_spec_no_equals() {
        assert_eq!(parse_env_spec("NOEQUALS"), None);
    }

    #[test]
    fn test_parse_env_spec_empty_key() {
        // =value is invalid (empty key)
        assert_eq!(parse_env_spec("=value"), None);
    }

    // === CLI Argument Parsing ===

    #[test]
    fn test_cli_defaults() {
        let cmd = parse_run(&["alpine:latest"]);
        assert_eq!(cmd.source, "alpine:latest");
        assert_eq!(cmd.memory, 512);
        assert_eq!(cmd.cpus, 1);
        assert!(!cmd.net);
        assert!(cmd.dns.is_none());
        assert!(cmd.name.is_none());
        assert!(cmd.workdir.is_none());
        assert!(cmd.command.is_empty());
        assert!(cmd.env.is_empty());
        assert!(cmd.volume.is_empty());
    }

    #[test]
    fn test_cli_with_command() {
        let cmd = parse_run(&["alpine", "/bin/echo", "hello", "world"]);
        assert_eq!(cmd.source, "alpine");
        assert_eq!(cmd.command, vec!["/bin/echo", "hello", "world"]);
    }

    #[test]
    fn test_cli_with_resources() {
        let cmd = parse_run(&["--memory", "1024", "--cpus", "4", "ubuntu:22.04"]);
        assert_eq!(cmd.memory, 1024);
        assert_eq!(cmd.cpus, 4);
        assert_eq!(cmd.source, "ubuntu:22.04");
    }

    #[test]
    fn test_cli_with_network() {
        let cmd = parse_run(&["--net", "--dns", "8.8.8.8", "alpine"]);
        assert!(cmd.net);
        assert_eq!(cmd.dns, Some("8.8.8.8".to_string()));
    }

    #[test]
    fn test_cli_with_env_vars() {
        let cmd = parse_run(&["-e", "FOO=bar", "-e", "BAZ=qux", "alpine"]);
        assert_eq!(cmd.env, vec!["FOO=bar", "BAZ=qux"]);
    }

    #[test]
    fn test_cli_with_volumes() {
        let cmd = parse_run(&["-v", "/host:/guest", "-v", "/data:/data:ro", "alpine"]);
        assert_eq!(cmd.volume, vec!["/host:/guest", "/data:/data:ro"]);
    }

    #[test]
    fn test_cli_with_name_and_workdir() {
        let cmd = parse_run(&["--name", "my-vm", "-w", "/app", "alpine"]);
        assert_eq!(cmd.name, Some("my-vm".to_string()));
        assert_eq!(cmd.workdir, Some("/app".to_string()));
    }

    #[test]
    fn test_cli_full_example() {
        let cmd = parse_run(&[
            "--name", "test-vm",
            "--memory", "2048",
            "--cpus", "2",
            "-w", "/workspace",
            "-e", "DEBUG=1",
            "-e", "PATH=/usr/bin",
            "-v", "/tmp:/tmp",
            "--net",
            "--dns", "1.1.1.1",
            "--console-log", "/tmp/console.log",
            "python:3.11",
            "python", "-c", "print('hello')",
        ]);
        assert_eq!(cmd.name, Some("test-vm".to_string()));
        assert_eq!(cmd.memory, 2048);
        assert_eq!(cmd.cpus, 2);
        assert_eq!(cmd.workdir, Some("/workspace".to_string()));
        assert_eq!(cmd.env, vec!["DEBUG=1", "PATH=/usr/bin"]);
        assert_eq!(cmd.volume, vec!["/tmp:/tmp"]);
        assert!(cmd.net);
        assert_eq!(cmd.dns, Some("1.1.1.1".to_string()));
        assert_eq!(cmd.console_log, Some("/tmp/console.log".to_string()));
        assert_eq!(cmd.source, "python:3.11");
        assert_eq!(cmd.command, vec!["python", "-c", "print('hello')"]);
    }
}
