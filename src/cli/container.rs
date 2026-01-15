//! Container lifecycle management commands.
//!
//! These commands manage long-running containers via a microvm.
//! Containers can be created, started, stopped, and deleted independently.

use clap::{Args, Subcommand};
use smolvm::agent::{AgentClient, AgentManager};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

/// Parse a duration string (e.g., "30s", "5m", "1h").
fn parse_duration(s: &str) -> Result<Duration, humantime::DurationError> {
    humantime::parse_duration(s)
}

/// Container management commands
#[derive(Subcommand, Debug)]
pub enum ContainerCmd {
    /// Create a new container from an image
    Create(ContainerCreateCmd),

    /// Start a created container
    Start(ContainerStartCmd),

    /// Stop a running container
    Stop(ContainerStopCmd),

    /// Remove a container
    #[command(alias = "rm")]
    Remove(ContainerRemoveCmd),

    /// List all containers
    #[command(alias = "ls")]
    List(ContainerListCmd),

    /// Execute a command in a running container
    Exec(ContainerExecCmd),
}

impl ContainerCmd {
    pub fn run(self) -> smolvm::Result<()> {
        match self {
            ContainerCmd::Create(cmd) => cmd.run(),
            ContainerCmd::Start(cmd) => cmd.run(),
            ContainerCmd::Stop(cmd) => cmd.run(),
            ContainerCmd::Remove(cmd) => cmd.run(),
            ContainerCmd::List(cmd) => cmd.run(),
            ContainerCmd::Exec(cmd) => cmd.run(),
        }
    }
}

/// Get the agent manager for a microvm, ensuring it's running
fn ensure_microvm(name: &str) -> smolvm::Result<AgentManager> {
    // "default" refers to the anonymous default microvm
    let manager = if name == "default" {
        AgentManager::default()?
    } else {
        AgentManager::for_vm(name)?
    };

    // Ensure microvm is running
    if manager.try_connect_existing().is_none() {
        println!("Starting microvm '{}'...", name);
        manager.ensure_running()?;
    }

    Ok(manager)
}

// ============================================================================
// Create
// ============================================================================

/// Create a new container from an image
#[derive(Args, Debug)]
pub struct ContainerCreateCmd {
    /// Target microvm name
    pub microvm: String,

    /// OCI image reference
    pub image: String,

    /// Command to run in the container (default: ["sleep", "infinity"])
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,

    /// Working directory inside container
    #[arg(short = 'w', long)]
    pub workdir: Option<String>,

    /// Environment variable (KEY=VALUE)
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,

    /// Volume mount (host:container[:ro])
    #[arg(short = 'v', long = "volume")]
    pub volume: Vec<String>,
}

impl ContainerCreateCmd {
    pub fn run(self) -> smolvm::Result<()> {
        let manager = ensure_microvm(&self.microvm)?;

        // Connect to agent
        let mut client = AgentClient::connect(manager.vsock_socket())?;

        // Pull image if needed
        if !std::path::Path::new(&self.image).exists() {
            println!("Pulling image {}...", self.image);
            client.pull(&self.image, None)?;
        }

        // Parse environment variables
        let env = parse_env(&self.env);

        // Parse mounts
        let mounts = parse_mounts_to_bindings(&self.volume)?;

        // Default command is sleep infinity for long-running containers
        let command = if self.command.is_empty() {
            vec!["sleep".to_string(), "infinity".to_string()]
        } else {
            self.command.clone()
        };

        // Create container
        let info = client.create_container(&self.image, command, env, self.workdir.clone(), mounts)?;

        println!("Created container: {}", info.id);
        println!("  Image: {}", info.image);
        println!("  State: {}", info.state);

        // Keep microvm running
        std::mem::forget(manager);

        Ok(())
    }
}

// ============================================================================
// Start
// ============================================================================

/// Start a created container
#[derive(Args, Debug)]
pub struct ContainerStartCmd {
    /// Target microvm name
    pub microvm: String,

    /// Container ID (full or prefix)
    pub container_id: String,
}

impl ContainerStartCmd {
    pub fn run(self) -> smolvm::Result<()> {
        let manager = ensure_microvm(&self.microvm)?;
        let mut client = AgentClient::connect(manager.vsock_socket())?;

        client.start_container(&self.container_id)?;
        println!("Started container: {}", self.container_id);

        // Keep microvm running
        std::mem::forget(manager);

        Ok(())
    }
}

// ============================================================================
// Stop
// ============================================================================

/// Stop a running container
#[derive(Args, Debug)]
pub struct ContainerStopCmd {
    /// Target microvm name
    pub microvm: String,

    /// Container ID (full or prefix)
    pub container_id: String,

    /// Timeout before force kill (default: 10s)
    #[arg(short = 't', long, value_parser = parse_duration)]
    pub timeout: Option<Duration>,
}

impl ContainerStopCmd {
    pub fn run(self) -> smolvm::Result<()> {
        let manager = ensure_microvm(&self.microvm)?;
        let mut client = AgentClient::connect(manager.vsock_socket())?;

        let timeout_secs = self.timeout.map(|d| d.as_secs());
        client.stop_container(&self.container_id, timeout_secs)?;
        println!("Stopped container: {}", self.container_id);

        // Keep microvm running
        std::mem::forget(manager);

        Ok(())
    }
}

// ============================================================================
// Remove
// ============================================================================

/// Remove a container
#[derive(Args, Debug)]
pub struct ContainerRemoveCmd {
    /// Target microvm name
    pub microvm: String,

    /// Container ID (full or prefix)
    pub container_id: String,

    /// Force removal even if running
    #[arg(short = 'f', long)]
    pub force: bool,
}

impl ContainerRemoveCmd {
    pub fn run(self) -> smolvm::Result<()> {
        let manager = ensure_microvm(&self.microvm)?;
        let mut client = AgentClient::connect(manager.vsock_socket())?;

        client.delete_container(&self.container_id, self.force)?;
        println!("Removed container: {}", self.container_id);

        // Keep microvm running
        std::mem::forget(manager);

        Ok(())
    }
}

// ============================================================================
// List
// ============================================================================

/// List all containers
#[derive(Args, Debug)]
pub struct ContainerListCmd {
    /// Target microvm name
    pub microvm: String,

    /// Show all containers (including stopped)
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Only display container IDs
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

impl ContainerListCmd {
    pub fn run(self) -> smolvm::Result<()> {
        // "default" refers to the anonymous default microvm
        let manager = if self.microvm == "default" {
            AgentManager::default()?
        } else {
            AgentManager::for_vm(&self.microvm)?
        };

        // Check if microvm is running
        if manager.try_connect_existing().is_none() {
            if self.quiet {
                return Ok(());
            }
            println!("No containers (microvm '{}' not running)", self.microvm);
            return Ok(());
        }

        let mut client = AgentClient::connect(manager.vsock_socket())?;
        let containers = client.list_containers()?;

        if self.quiet {
            // Just print IDs
            for c in &containers {
                if self.all || c.state == "running" {
                    println!("{}", c.id);
                }
            }
        } else if containers.is_empty() {
            println!("No containers");
        } else {
            // Table format
            println!(
                "{:<16} {:<20} {:<12} {:<30}",
                "CONTAINER ID", "IMAGE", "STATE", "COMMAND"
            );

            for c in &containers {
                if !self.all && c.state != "running" {
                    continue;
                }

                // Truncate container ID for display
                let short_id = if c.id.len() > 12 {
                    &c.id[..12]
                } else {
                    &c.id
                };

                // Truncate image name for display
                let short_image = if c.image.len() > 18 {
                    format!("{}...", &c.image[..15])
                } else {
                    c.image.clone()
                };

                // Format command
                let cmd_str = c.command.join(" ");
                let short_cmd = if cmd_str.len() > 28 {
                    format!("{}...", &cmd_str[..25])
                } else {
                    cmd_str
                };

                println!(
                    "{:<16} {:<20} {:<12} {:<30}",
                    short_id, short_image, c.state, short_cmd
                );
            }
        }

        // Keep microvm running
        std::mem::forget(manager);

        Ok(())
    }
}

// ============================================================================
// Exec
// ============================================================================

/// Execute a command in a running container
#[derive(Args, Debug)]
pub struct ContainerExecCmd {
    /// Target microvm name
    pub microvm: String,

    /// Container ID (full or prefix)
    pub container_id: String,

    /// Command to execute
    #[arg(trailing_var_arg = true)]
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
}

impl ContainerExecCmd {
    pub fn run(self) -> smolvm::Result<()> {
        let manager = ensure_microvm(&self.microvm)?;
        let mut client = AgentClient::connect(manager.vsock_socket())?;

        // Parse environment variables
        let env = parse_env(&self.env);

        // Default command
        let command = if self.command.is_empty() {
            vec!["/bin/sh".to_string()]
        } else {
            self.command.clone()
        };

        // Execute in container
        let (exit_code, stdout, stderr) =
            client.exec(&self.container_id, command, env, self.workdir.clone(), self.timeout)?;

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

        // Keep microvm running
        std::mem::forget(manager);

        std::process::exit(exit_code);
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Parse environment variables from CLI args
fn parse_env(env_args: &[String]) -> Vec<(String, String)> {
    env_args
        .iter()
        .filter_map(|e| {
            let (k, v) = e.split_once('=')?;
            if k.is_empty() {
                None
            } else {
                Some((k.to_string(), v.to_string()))
            }
        })
        .collect()
}

/// Parse volume mounts and convert to virtiofs bindings
fn parse_mounts_to_bindings(volume_args: &[String]) -> smolvm::Result<Vec<(String, String, bool)>> {
    use smolvm::Error;

    let mut bindings = Vec::new();

    for (i, spec) in volume_args.iter().enumerate() {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 2 {
            return Err(Error::Mount(format!(
                "invalid volume specification '{}': expected host:container[:ro]",
                spec
            )));
        }

        let host_path = PathBuf::from(parts[0]);
        let guest_path = parts[1].to_string();
        let read_only = parts.get(2).map(|&s| s == "ro").unwrap_or(false);

        // Validate host path exists
        if !host_path.exists() {
            return Err(Error::Mount(format!(
                "host path does not exist: {}",
                host_path.display()
            )));
        }

        // Use virtiofs tag format
        let tag = format!("smolvm{}", i);
        bindings.push((tag, guest_path, read_only));
    }

    Ok(bindings)
}
