//! Start command implementation.

use clap::Args;
use smolvm::config::{RecordState, SmolvmConfig};
use smolvm::{default_backend, NetworkPolicy, RootfsSource, VmConfig, VmId};
use std::io::Write;
use std::path::PathBuf;

/// Get the runtime directory for PID files.
fn runtime_dir() -> PathBuf {
    // Use XDG_RUNTIME_DIR or fallback to /tmp
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("smolvm")
}

/// Start a created or stopped VM.
#[derive(Args, Debug)]
pub struct StartCmd {
    /// VM name to start.
    pub name: String,

    /// Run in foreground (don't daemonize).
    #[arg(long)]
    pub foreground: bool,
}

impl StartCmd {
    /// Execute the start command.
    pub fn run(self, config: &mut SmolvmConfig) -> smolvm::Result<()> {
        // Get VM record
        let record = config
            .get_vm(&self.name)
            .ok_or_else(|| smolvm::Error::VmNotFound(self.name.clone()))?
            .clone();

        // Check state
        let actual_state = record.actual_state();
        if actual_state == RecordState::Running {
            return Err(smolvm::Error::InvalidState {
                expected: "created or stopped".to_string(),
                actual: "running".to_string(),
            });
        }

        // Get rootfs
        let rootfs = if let Some(ref cid) = record.container_id {
            RootfsSource::buildah(cid)
        } else {
            RootfsSource::path(&record.rootfs_source)
        };

        // Build network policy
        let network = if record.net_enabled {
            let dns = record.dns.as_ref().and_then(|d| d.parse().ok());
            NetworkPolicy::Egress { dns }
        } else {
            NetworkPolicy::None
        };

        // Build VM config from record
        let mut builder = VmConfig::builder(rootfs)
            .id(VmId::new(&record.name))
            .memory(record.mem)
            .cpus(record.cpus)
            .network(network);

        // Set command if specified
        if let Some(ref cmd) = record.command {
            if !cmd.is_empty() {
                builder = builder.command(cmd.clone());
            }
        }

        // Set working directory if specified
        if let Some(ref wd) = record.workdir {
            builder = builder.workdir(wd);
        }

        // Add environment variables
        for (k, v) in &record.env {
            builder = builder.env(k.clone(), v.clone());
        }

        let vm_config = builder.build();

        if self.foreground {
            // Run in foreground (like `run` command)
            self.run_foreground(config, vm_config)
        } else {
            // Daemonize
            self.run_daemon(config, vm_config)
        }
    }

    fn run_foreground(self, config: &mut SmolvmConfig, vm_config: VmConfig) -> smolvm::Result<()> {
        let vm_id = vm_config.id.clone();

        // Update state to running
        config.update_vm(&self.name, |r| {
            r.state = RecordState::Running;
            r.pid = Some(std::process::id() as i32);
        });
        config.save()?;

        println!("Starting VM {} (foreground)...", vm_id);

        let backend = default_backend()?;
        let mut vm = backend.create(vm_config)?;
        let exit = vm.wait()?;

        // Update state to stopped
        config.update_vm(&self.name, |r| {
            r.state = RecordState::Stopped;
            r.pid = None;
        });
        config.save()?;

        tracing::info!(vm_id = %vm_id, exit = ?exit, "VM exited");
        std::process::exit(exit.exit_code());
    }

    fn run_daemon(self, config: &mut SmolvmConfig, vm_config: VmConfig) -> smolvm::Result<()> {
        let vm_id = vm_config.id.clone();

        // Create runtime directory
        let runtime = runtime_dir();
        std::fs::create_dir_all(&runtime)?;

        let pid_file = runtime.join(format!("{}.pid", self.name));
        let log_file = runtime.join(format!("{}.log", self.name));

        // Double-fork to daemonize
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(smolvm::Error::vm_creation("first fork failed"));
        }

        if pid == 0 {
            // First child: create new session
            unsafe { libc::setsid() };

            // Fork again
            let pid2 = unsafe { libc::fork() };
            if pid2 < 0 {
                unsafe { libc::_exit(1) };
            }
            if pid2 > 0 {
                // First child exits, orphaning grandchild to init
                unsafe { libc::_exit(0) };
            }

            // Grandchild: this is the daemon
            // Redirect stdout/stderr to log file
            if let Ok(log) = std::fs::File::create(&log_file) {
                unsafe {
                    libc::dup2(log.as_raw_fd(), libc::STDOUT_FILENO);
                    libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO);
                }
            }

            // Write PID file
            if let Ok(mut f) = std::fs::File::create(&pid_file) {
                let _ = writeln!(f, "{}", std::process::id());
            }

            // Run VM
            let backend = match default_backend() {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("Failed to get backend: {}", e);
                    unsafe { libc::_exit(1) };
                }
            };

            let mut vm = match backend.create(vm_config) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Failed to create VM: {}", e);
                    unsafe { libc::_exit(1) };
                }
            };

            let exit = match vm.wait() {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("VM error: {}", e);
                    unsafe { libc::_exit(1) };
                }
            };

            // Clean up PID file
            let _ = std::fs::remove_file(&pid_file);

            unsafe { libc::_exit(exit.exit_code()) };
        }

        // Parent: wait for first child to exit
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };

        // Read daemon PID from file (with retry)
        let daemon_pid = self.read_pid_file_with_retry(&pid_file)?;

        // Update config
        config.update_vm(&self.name, |r| {
            r.state = RecordState::Running;
            r.pid = Some(daemon_pid);
            r.pid_file = Some(pid_file.to_string_lossy().to_string());
        });
        config.save()?;

        println!("Started VM: {} (PID: {})", vm_id, daemon_pid);
        println!("Logs: {}", log_file.display());

        Ok(())
    }

    fn read_pid_file_with_retry(&self, path: &PathBuf) -> smolvm::Result<i32> {
        use std::time::{Duration, Instant};

        let timeout = Duration::from_secs(5);
        let start = Instant::now();

        loop {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    return Ok(pid);
                }
            }

            if start.elapsed() > timeout {
                return Err(smolvm::Error::vm_creation(
                    "timeout waiting for daemon PID file",
                ));
            }

            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

use std::os::unix::io::AsRawFd;
