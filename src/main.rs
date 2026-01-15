//! smolvm CLI entry point.

use clap::{Parser, Subcommand};
use smolvm::config::SmolvmConfig;
use tracing_subscriber::EnvFilter;

mod cli;

/// smolvm - OCI-native microVM runtime
#[derive(Parser, Debug)]
#[command(name = "smolvm")]
#[command(about = "OCI-native microVM runtime")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run a VM from a rootfs path or OCI image (ephemeral).
    Run(cli::run::RunCmd),

    /// Create a VM without starting it.
    Create(cli::create::CreateCmd),

    /// Start a created/stopped VM.
    Start(cli::start::StartCmd),

    /// Stop a running VM.
    Stop(cli::stop::StopCmd),

    /// List all VMs.
    #[command(alias = "ls")]
    List(cli::list::ListCmd),

    /// Delete a VM.
    #[command(alias = "rm")]
    Delete(cli::delete::DeleteCmd),

    /// Manage microvms.
    #[command(subcommand)]
    Microvm(cli::microvm::MicrovmCmd),

    /// Execute a command in a container via the agent VM.
    Exec(cli::exec::ExecCmd),

    /// Manage containers in the agent VM.
    #[command(subcommand)]
    Container(cli::container::ContainerCmd),
}

fn main() {
    let cli = Cli::parse();

    // Initialize logging based on RUST_LOG or default to warn
    init_logging();

    tracing::debug!(version = smolvm::VERSION, "starting smolvm");

    // Load configuration
    let mut config = match SmolvmConfig::load() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load config, using defaults");
            SmolvmConfig::default()
        }
    };

    // Execute command
    let result = match cli.command {
        Commands::Run(cmd) => cmd.run(&mut config),
        Commands::Create(cmd) => cmd.run(&mut config),
        Commands::Start(cmd) => cmd.run(&mut config),
        Commands::Stop(cmd) => cmd.run(&mut config),
        Commands::List(cmd) => cmd.run(&config),
        Commands::Delete(cmd) => cmd.run(&mut config),
        Commands::Microvm(cmd) => cmd.run(),
        Commands::Exec(cmd) => cmd.run(),
        Commands::Container(cmd) => cmd.run(),
    };

    // Handle errors
    if let Err(e) = result {
        tracing::error!(error = %e, "command failed");
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    // Save configuration
    if let Err(e) = config.save() {
        tracing::warn!(error = %e, "failed to save config");
    }
}

/// Initialize the tracing subscriber.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("smolvm=warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
