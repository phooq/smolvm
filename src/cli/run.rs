//! Run command implementation.

use clap::Args;
use smolvm::agent::{AgentClient, AgentManager, HostMount, PortMapping, VmResources};
use smolvm::config::SmolvmConfig;
use std::path::PathBuf;
use std::time::Duration;

/// Parse a duration string (e.g., "30s", "5m", "1h").
fn parse_duration(s: &str) -> Result<Duration, humantime::DurationError> {
    humantime::parse_duration(s)
}

/// Parse a port mapping specification (HOST:GUEST or PORT).
fn parse_port(s: &str) -> Result<PortMapping, String> {
    if let Some((host, guest)) = s.split_once(':') {
        let host: u16 = host.parse().map_err(|_| format!("invalid host port: {}", host))?;
        let guest: u16 = guest.parse().map_err(|_| format!("invalid guest port: {}", guest))?;
        Ok(PortMapping::new(host, guest))
    } else {
        let port: u16 = s.parse().map_err(|_| format!("invalid port: {}", s))?;
        Ok(PortMapping::same(port))
    }
}

/// Parse an environment variable specification (KEY=VALUE).
fn parse_env_spec(spec: &str) -> Option<(String, String)> {
    let (key, value) = spec.split_once('=')?;
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// Run a command in a container (ephemeral).
///
/// Unlike `exec`, this stops the agent VM after the command completes.
#[derive(Args, Debug)]
pub struct RunCmd {
    /// OCI image reference.
    pub source: String,

    /// Command to execute inside the container.
    ///
    /// If not specified, defaults to /bin/sh.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,

    /// VM name (for identification only).
    #[arg(long)]
    pub name: Option<String>,

    /// Memory in MiB.
    #[arg(long, default_value = "512")]
    pub memory: u32,

    /// Number of vCPUs.
    #[arg(long, default_value = "1")]
    pub cpus: u8,

    /// Working directory inside the container.
    #[arg(short = 'w', long)]
    pub workdir: Option<String>,

    /// Environment variable (KEY=VALUE).
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,

    /// Volume mount (host:guest[:ro]).
    #[arg(short = 'v', long = "volume")]
    pub volume: Vec<String>,

    /// Enable network egress (auto-enabled when -p is used).
    #[arg(long)]
    pub net: bool,

    /// Custom DNS server (requires --net).
    #[arg(long)]
    pub dns: Option<String>,

    /// Port mapping from host to guest (HOST:GUEST or PORT).
    ///
    /// Examples: -p 8080:80, -p 3000. Enables network egress automatically.
    #[arg(short = 'p', long = "port", value_parser = parse_port)]
    pub port: Vec<PortMapping>,

    /// Timeout for command execution (e.g., "30s", "5m").
    ///
    /// If the command exceeds this duration, it will be killed
    /// and exit with code 124.
    #[arg(long, value_parser = parse_duration)]
    pub timeout: Option<std::time::Duration>,

    /// Keep stdin open (interactive mode).
    ///
    /// Allows sending input to the container. Combine with -t for a full terminal.
    #[arg(short = 'i', long)]
    pub interactive: bool,

    /// Allocate a pseudo-TTY.
    ///
    /// Enables terminal features like colors and line editing. Usually combined with -i.
    #[arg(short = 't', long)]
    pub tty: bool,
}

impl RunCmd {
    /// Execute the run command.
    pub fn run(self, _config: &mut SmolvmConfig) -> smolvm::Result<()> {
        use smolvm::Error;
        use std::io::Write;

        // Parse volume mounts
        let mounts = self.parse_mounts()?;

        // Get port mappings
        let ports = self.port.clone();

        // Start agent VM
        let manager = AgentManager::default().map_err(|e| {
            Error::AgentError(format!("failed to create agent manager: {}", e))
        })?;

        let resources = VmResources {
            cpus: self.cpus,
            mem: self.memory,
        };

        // Show startup message
        let mount_info = if !mounts.is_empty() {
            format!(" with {} mount(s)", mounts.len())
        } else {
            String::new()
        };
        let port_info = if !ports.is_empty() {
            format!(" and {} port mapping(s)", ports.len())
        } else {
            String::new()
        };
        println!("Starting agent VM{}{}...", mount_info, port_info);

        manager.ensure_running_with_full_config(mounts.clone(), ports, resources).map_err(|e| {
            Error::AgentError(format!("failed to start agent: {}", e))
        })?;

        // Connect to agent
        let mut client = AgentClient::connect(manager.vsock_socket())?;

        // Pull image if not a local path
        if !std::path::Path::new(&self.source).exists() {
            println!("Pulling image {}...", self.source);
            client.pull(&self.source, None)?;
        }

        // Build command
        let command = if self.command.is_empty() {
            vec!["/bin/sh".to_string()]
        } else {
            self.command.clone()
        };

        // Parse environment variables
        let env: Vec<(String, String)> = self
            .env
            .iter()
            .filter_map(|e| parse_env_spec(e))
            .collect();

        // Convert mounts to agent format
        let mount_bindings: Vec<(String, String, bool)> = mounts
            .iter()
            .enumerate()
            .map(|(i, m)| {
                (
                    format!("smolvm{}", i),
                    m.guest_path.to_string_lossy().to_string(),
                    m.read_only,
                )
            })
            .collect();

        // Run command
        let exit_code = if self.interactive || self.tty {
            // Interactive mode - stream I/O
            client.run_interactive(
                &self.source,
                command,
                env,
                self.workdir.clone(),
                mount_bindings,
                self.timeout,
                self.tty,
            )?
        } else {
            // Non-interactive mode - buffer output
            let (exit_code, stdout, stderr) = client.run_with_mounts_and_timeout(
                &self.source,
                command,
                env,
                self.workdir.clone(),
                mount_bindings,
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

        // Stop the agent VM (ephemeral mode - unlike exec which keeps it running)
        if let Err(e) = manager.stop() {
            tracing::warn!(error = %e, "failed to stop agent");
        }

        std::process::exit(exit_code);
    }

    /// Parse volume mount specifications.
    fn parse_mounts(&self) -> smolvm::Result<Vec<HostMount>> {
        use smolvm::Error;

        let mut mounts = Vec::new();

        for spec in &self.volume {
            let parts: Vec<&str> = spec.split(':').collect();
            if parts.len() < 2 {
                return Err(Error::Mount(format!(
                    "invalid volume specification '{}': expected host:container[:ro]",
                    spec
                )));
            }

            let host_path = PathBuf::from(parts[0]);
            let guest_path = PathBuf::from(parts[1]);
            let read_only = parts.get(2).map(|&s| s == "ro").unwrap_or(false);

            // Validate host path exists
            if !host_path.exists() {
                return Err(Error::Mount(format!(
                    "host path does not exist: {}",
                    host_path.display()
                )));
            }

            // Must be a directory (virtiofs limitation)
            if !host_path.is_dir() {
                return Err(Error::Mount(format!(
                    "host path must be a directory (virtiofs limitation): {}",
                    host_path.display()
                )));
            }

            // Canonicalize host path
            let host_path = host_path.canonicalize().map_err(|e| {
                Error::Mount(format!(
                    "failed to resolve host path '{}': {}",
                    parts[0], e
                ))
            })?;

            mounts.push(HostMount {
                host_path,
                guest_path,
                read_only,
            });
        }

        Ok(mounts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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

    #[test]
    fn test_parse_env_spec_valid() {
        assert_eq!(
            parse_env_spec("FOO=bar"),
            Some(("FOO".to_string(), "bar".to_string()))
        );
    }

    #[test]
    fn test_parse_env_spec_empty_value() {
        assert_eq!(
            parse_env_spec("FOO="),
            Some(("FOO".to_string(), "".to_string()))
        );
    }

    #[test]
    fn test_parse_env_spec_value_with_equals() {
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
        assert_eq!(parse_env_spec("=value"), None);
    }

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
    fn test_cli_with_env_vars() {
        let cmd = parse_run(&["-e", "FOO=bar", "-e", "BAZ=qux", "alpine"]);
        assert_eq!(cmd.env, vec!["FOO=bar", "BAZ=qux"]);
    }

    #[test]
    fn test_cli_with_volumes() {
        let cmd = parse_run(&["-v", "/host:/guest", "-v", "/data:/data:ro", "alpine"]);
        assert_eq!(cmd.volume, vec!["/host:/guest", "/data:/data:ro"]);
    }
}
