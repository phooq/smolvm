//! MicroVM management commands.

use clap::{Args, Subcommand};
use smolvm::agent::{AgentClient, AgentManager};
use smolvm::config::SmolvmConfig;
use std::time::Duration;

/// Parse a duration string (e.g., "30s", "5m", "1h").
fn parse_duration(s: &str) -> Result<Duration, humantime::DurationError> {
    humantime::parse_duration(s)
}

/// Manage microvms
#[derive(Subcommand, Debug)]
pub enum MicrovmCmd {
    /// Start a microvm
    Start(MicrovmStartCmd),
    /// Stop a microvm
    Stop(MicrovmStopCmd),
    /// Show microvm status
    Status(MicrovmStatusCmd),
    /// List all microvms
    #[command(alias = "list")]
    Ls(MicrovmLsCmd),
    /// Execute a command in a microvm
    Exec(MicrovmExecCmd),
    /// Test network connectivity (debug TSI)
    NetworkTest(MicrovmNetworkTestCmd),
}

impl MicrovmCmd {
    pub fn run(self) -> smolvm::Result<()> {
        match self {
            MicrovmCmd::Start(cmd) => cmd.run(),
            MicrovmCmd::Stop(cmd) => cmd.run(),
            MicrovmCmd::Status(cmd) => cmd.run(),
            MicrovmCmd::Ls(cmd) => cmd.run(),
            MicrovmCmd::Exec(cmd) => cmd.run(),
            MicrovmCmd::NetworkTest(cmd) => cmd.run(),
        }
    }
}

/// Get the agent manager for a name (or default if None)
fn get_manager(name: &Option<String>) -> smolvm::Result<AgentManager> {
    if let Some(name) = name {
        AgentManager::for_vm(name)
    } else {
        AgentManager::default()
    }
}

/// Format the microvm label for display
fn microvm_label(name: &Option<String>) -> String {
    name.as_deref().unwrap_or("default").to_string()
}

/// Start a microvm
#[derive(Args, Debug)]
pub struct MicrovmStartCmd {
    /// Named microvm to start (default: anonymous)
    #[arg(long)]
    pub name: Option<String>,
}

impl MicrovmStartCmd {
    pub fn run(self) -> smolvm::Result<()> {
        let manager = get_manager(&self.name)?;
        let label = microvm_label(&self.name);

        // Check if already running
        if manager.try_connect_existing().is_some() {
            println!("MicroVM '{}' already running", label);
            // Don't stop - stays running
            std::mem::forget(manager);
            return Ok(());
        }

        println!("Starting microvm '{}'...", label);
        manager.ensure_running()?;

        let pid = manager.child_pid().unwrap_or(0);
        println!("MicroVM '{}' running (PID: {})", label, pid);

        // Don't stop - stays running
        std::mem::forget(manager);

        Ok(())
    }
}

/// Stop a microvm
#[derive(Args, Debug)]
pub struct MicrovmStopCmd {
    /// Named microvm to stop (default: anonymous)
    #[arg(long)]
    pub name: Option<String>,
}

impl MicrovmStopCmd {
    pub fn run(self) -> smolvm::Result<()> {
        let manager = get_manager(&self.name)?;
        let label = microvm_label(&self.name);

        if manager.try_connect_existing().is_some() {
            println!("Stopping microvm '{}'...", label);
            manager.stop()?;
            println!("MicroVM '{}' stopped", label);
        } else {
            println!("MicroVM '{}' not running", label);
        }

        Ok(())
    }
}

/// Show microvm status
#[derive(Args, Debug)]
pub struct MicrovmStatusCmd {
    /// Named microvm to check (default: anonymous)
    #[arg(long)]
    pub name: Option<String>,
}

impl MicrovmStatusCmd {
    pub fn run(self) -> smolvm::Result<()> {
        let manager = get_manager(&self.name)?;
        let label = microvm_label(&self.name);

        if manager.try_connect_existing().is_some() {
            let pid = manager.child_pid().map(|p| format!(" (PID: {})", p)).unwrap_or_default();
            println!("MicroVM '{}': running{}", label, pid);
            // Don't stop - just checking status
            std::mem::forget(manager);
        } else {
            println!("MicroVM '{}': stopped", label);
        }

        Ok(())
    }
}

/// Test network connectivity directly from microvm (debug TSI)
#[derive(Args, Debug)]
pub struct MicrovmNetworkTestCmd {
    /// Named microvm to test (default: anonymous)
    #[arg(long)]
    pub name: Option<String>,

    /// URL to test (default: http://1.1.1.1)
    #[arg(default_value = "http://1.1.1.1")]
    pub url: String,
}

impl MicrovmNetworkTestCmd {
    pub fn run(self) -> smolvm::Result<()> {
        let manager = get_manager(&self.name)?;
        let label = microvm_label(&self.name);

        // Ensure microvm is running
        if manager.try_connect_existing().is_none() {
            println!("Starting microvm '{}'...", label);
            manager.ensure_running()?;
        }

        // Connect and test
        println!("Testing network from microvm: {}", self.url);
        let mut client = manager.connect()?;
        let result = client.network_test(&self.url)?;

        println!("Result: {}", serde_json::to_string_pretty(&result).unwrap_or_default());

        // Don't stop - keep running
        std::mem::forget(manager);

        Ok(())
    }
}

/// List all microvms
#[derive(Args, Debug)]
pub struct MicrovmLsCmd {
    /// Show detailed output
    #[arg(short, long)]
    pub verbose: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl MicrovmLsCmd {
    pub fn run(&self) -> smolvm::Result<()> {
        // Load config to get VM list
        let config = SmolvmConfig::load().unwrap_or_default();
        let vms: Vec<_> = config.list_vms().collect();

        if vms.is_empty() {
            if !self.json {
                println!("No microvms found");
            } else {
                println!("[]");
            }
            return Ok(());
        }

        if self.json {
            let json_vms: Vec<_> = vms
                .iter()
                .map(|(name, record)| {
                    let actual_state = record.actual_state();
                    serde_json::json!({
                        "name": name,
                        "state": actual_state.to_string(),
                        "image": record.image,
                        "cpus": record.cpus,
                        "memory_mib": record.mem,
                        "pid": record.pid,
                        "command": record.command,
                        "workdir": record.workdir,
                        "env": record.env,
                        "mounts": record.mounts.len(),
                        "created_at": record.created_at,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json_vms).unwrap());
        } else {
            // Table output
            println!(
                "{:<20} {:<10} {:<5} {:<8} {:<25} {:<6}",
                "NAME", "STATE", "CPUS", "MEMORY", "IMAGE", "MOUNTS"
            );
            println!("{}", "-".repeat(78));

            for (name, record) in vms {
                let actual_state = record.actual_state();

                println!(
                    "{:<20} {:<10} {:<5} {:<8} {:<25} {:<6}",
                    truncate(name, 18),
                    actual_state,
                    record.cpus,
                    format!("{} MiB", record.mem),
                    truncate(&record.image, 23),
                    record.mounts.len(),
                );

                if self.verbose {
                    if let Some(cmd) = &record.command {
                        println!("  Command: {:?}", cmd);
                    }
                    if let Some(wd) = &record.workdir {
                        println!("  Workdir: {}", wd);
                    }
                    if !record.env.is_empty() {
                        println!("  Env: {} variable(s)", record.env.len());
                    }
                    for (host, guest, ro) in &record.mounts {
                        let ro_str = if *ro { " (ro)" } else { "" };
                        println!("  Mount: {} -> {}{}", host, guest, ro_str);
                    }
                    println!("  Created: {}", record.created_at);
                    println!();
                }
            }
        }

        Ok(())
    }
}

/// Truncate a string to max length, adding "..." if needed.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

/// Execute a command in a microvm
#[derive(Args, Debug)]
pub struct MicrovmExecCmd {
    /// Microvm name (default: anonymous default microvm)
    #[arg(long)]
    pub name: Option<String>,

    /// OCI image to use (default: alpine:latest)
    #[arg(long, default_value = "alpine:latest")]
    pub image: String,

    /// Command to execute
    #[arg(trailing_var_arg = true, required = true)]
    pub command: Vec<String>,

    /// Working directory inside container
    #[arg(short = 'w', long)]
    pub workdir: Option<String>,

    /// Environment variable (KEY=VALUE)
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,

    /// Timeout for command execution (e.g., "30s", "5m")
    #[arg(long, value_parser = parse_duration)]
    pub timeout: Option<Duration>,

    /// Keep stdin open (interactive mode)
    #[arg(short = 'i', long)]
    pub interactive: bool,

    /// Allocate a pseudo-TTY
    #[arg(short = 't', long)]
    pub tty: bool,
}

impl MicrovmExecCmd {
    pub fn run(self) -> smolvm::Result<()> {
        use std::io::Write;

        let manager = get_manager(&self.name)?;
        let label = microvm_label(&self.name);

        // Ensure microvm is running
        let was_running = manager.try_connect_existing().is_some();
        if !was_running {
            println!("Starting microvm '{}'...", label);
            manager.ensure_running()?;
        }

        // Connect to agent
        let mut client = AgentClient::connect(manager.vsock_socket())?;

        // Pull image if needed
        if !was_running {
            println!("Pulling image {}...", self.image);
        }
        client.pull(&self.image, None)?;

        // Parse environment variables
        let env: Vec<(String, String)> = self
            .env
            .iter()
            .filter_map(|e| {
                let (k, v) = e.split_once('=')?;
                if k.is_empty() {
                    None
                } else {
                    Some((k.to_string(), v.to_string()))
                }
            })
            .collect();

        // Run command
        let exit_code = if self.interactive || self.tty {
            // Interactive mode - stream I/O
            client.run_interactive(
                &self.image,
                self.command.clone(),
                env,
                self.workdir.clone(),
                vec![], // No mounts for microvm exec
                self.timeout,
                self.tty,
            )?
        } else {
            // Non-interactive mode - buffer output
            let (exit_code, stdout, stderr) = client.run_with_mounts_and_timeout(
                &self.image,
                self.command.clone(),
                env,
                self.workdir.clone(),
                vec![], // No mounts for microvm exec
                self.timeout,
            )?;

            // Print output
            if !stdout.is_empty() {
                print!("{}", stdout);
            }
            if !stderr.is_empty() {
                eprint!("{}", stderr);
            }

            // Flush output
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();

            exit_code
        };

        // Keep microvm running
        std::mem::forget(manager);

        std::process::exit(exit_code);
    }
}
