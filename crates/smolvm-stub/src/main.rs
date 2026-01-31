//! Stub executable for packed smolvm binaries.
//!
//! This is the entry point for packed binaries. It:
//! 1. Reads the footer and manifest from itself
//! 2. Extracts assets to a cache directory if needed
//! 3. Launches the VM with the packaged image
//!
//! Supports three modes:
//! - Sidecar: Assets in .smolmachine file (default, best for macOS signing)
//! - Embedded (section): Assets in __DATA,__smolvm Mach-O section (macOS signed single-file)
//! - Embedded (append): Assets appended to binary (Linux single-file)
//!
//! And two runtime modes:
//! - Ephemeral (default): Boot VM, run command, exit
//! - Daemon: Keep VM running for fast repeated exec

mod extract;
mod launch;
#[cfg(target_os = "macos")]
mod section;

use clap::{Parser, Subcommand};
use smolvm_pack::format::PackManifest;
use smolvm_pack::packer::{read_footer, read_manifest};
use std::env;
use std::process::ExitCode;

/// Packed smolvm binary - runs a containerized application in a microVM.
#[derive(Parser, Debug)]
#[command(name = "packed-binary")]
#[command(about = "Run a containerized application in a microVM")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Command to run (overrides image entrypoint/cmd) - for ephemeral mode
    #[arg(trailing_var_arg = true, conflicts_with = "command")]
    run_command: Vec<String>,

    /// Mount a volume (HOST:GUEST[:ro])
    #[arg(short = 'v', long = "volume", value_name = "HOST:GUEST", global = true)]
    volumes: Vec<String>,

    /// Set environment variable (KEY=VALUE)
    #[arg(short = 'e', long = "env", value_name = "KEY=VALUE", global = true)]
    env: Vec<String>,

    /// Working directory inside the container
    #[arg(short = 'w', long = "workdir", value_name = "PATH", global = true)]
    workdir: Option<String>,

    /// Number of vCPUs (overrides default)
    #[arg(long, value_name = "N", global = true)]
    cpus: Option<u8>,

    /// Memory in MiB (overrides default)
    #[arg(long, value_name = "MiB", global = true)]
    mem: Option<u32>,

    /// Show version and exit
    #[arg(long)]
    version: bool,

    /// Show manifest info and exit
    #[arg(long)]
    info: bool,

    /// Force re-extraction of assets
    #[arg(long, global = true)]
    force_extract: bool,

    /// Print debug information
    #[arg(long, global = true)]
    debug: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start the VM daemon (keeps running for subsequent exec calls)
    Start,

    /// Execute a command in the running daemon VM (~50ms)
    Exec {
        /// Command to run
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },

    /// Stop the running daemon VM
    Stop,

    /// Check if the daemon VM is running
    Status,
}

/// Data source for packed assets.
enum PackedData {
    /// Assets embedded in Mach-O section (macOS single-file).
    #[cfg(target_os = "macos")]
    Section {
        manifest: PackManifest,
        checksum: u32,
        assets_ptr: *const u8,
        assets_size: usize,
    },
    /// Assets in sidecar file or appended to binary.
    FooterBased {
        manifest: PackManifest,
        footer: smolvm_pack::PackFooter,
    },
}

fn main() -> ExitCode {
    let args = Args::parse();

    // Get path to this executable
    let exe_path = match env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: failed to get executable path: {}", e);
            return ExitCode::from(1);
        }
    };

    // Try to read packed data - section first on macOS, then footer-based
    let packed_data = load_packed_data(&exe_path, args.debug);
    let packed_data = match packed_data {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {}", e);
            eprintln!("This binary may not be a valid packed smolvm executable.");
            return ExitCode::from(1);
        }
    };

    // Get manifest and checksum from packed data
    let (manifest, checksum) = match &packed_data {
        #[cfg(target_os = "macos")]
        PackedData::Section {
            manifest, checksum, ..
        } => (manifest.clone(), *checksum),
        PackedData::FooterBased { manifest, footer } => (manifest.clone(), footer.checksum),
    };

    if args.debug {
        match &packed_data {
            #[cfg(target_os = "macos")]
            PackedData::Section { assets_size, .. } => {
                eprintln!("debug: using Mach-O section mode");
                eprintln!("  assets_size: {}", assets_size);
                eprintln!("  checksum: {:08x}", checksum);
            }
            PackedData::FooterBased { footer, .. } => {
                eprintln!("debug: using footer-based mode");
                eprintln!("  stub_size: {}", footer.stub_size);
                eprintln!("  assets_offset: {}", footer.assets_offset);
                eprintln!("  assets_size: {}", footer.assets_size);
                eprintln!("  manifest_offset: {}", footer.manifest_offset);
                eprintln!("  manifest_size: {}", footer.manifest_size);
                eprintln!("  checksum: {:08x}", footer.checksum);
            }
        }
    }

    if args.version {
        println!("Packed smolvm binary for {}", manifest.image);
        println!("Digest: {}", manifest.digest);
        println!("Platform: {}", manifest.platform);
        return ExitCode::SUCCESS;
    }

    if args.info {
        println!("Image: {}", manifest.image);
        println!("Digest: {}", manifest.digest);
        println!("Platform: {}", manifest.platform);
        println!("Default CPUs: {}", manifest.cpus);
        println!("Default Memory: {} MiB", manifest.mem);
        if !manifest.entrypoint.is_empty() {
            println!("Entrypoint: {:?}", manifest.entrypoint);
        }
        if !manifest.cmd.is_empty() {
            println!("Cmd: {:?}", manifest.cmd);
        }
        if let Some(ref wd) = manifest.workdir {
            println!("Workdir: {}", wd);
        }
        println!("\nAssets:");
        println!("  Libraries: {}", manifest.assets.libraries.len());
        println!("  Layers: {}", manifest.assets.layers.len());
        return ExitCode::SUCCESS;
    }

    // Extract assets to cache directory
    let cache_dir = match extract::get_cache_dir(checksum) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: failed to determine cache directory: {}", e);
            return ExitCode::from(1);
        }
    };

    if args.debug {
        eprintln!("debug: cache directory: {}", cache_dir.display());
    }

    let needs_extract = args.force_extract || !extract::is_extracted(&cache_dir);

    if needs_extract {
        if args.debug {
            eprintln!("debug: extracting assets...");
        }

        let extract_result = match &packed_data {
            #[cfg(target_os = "macos")]
            PackedData::Section {
                assets_ptr,
                assets_size,
                ..
            } => extract::extract_from_section(&cache_dir, *assets_ptr, *assets_size, args.debug),
            PackedData::FooterBased { footer, .. } => {
                extract::extract_to_cache(&exe_path, &cache_dir, footer, args.debug)
            }
        };

        if let Err(e) = extract_result {
            eprintln!("error: failed to extract assets: {}", e);
            return ExitCode::from(1);
        }

        if args.debug {
            eprintln!("debug: extraction complete");
        }
    } else if args.debug {
        eprintln!("debug: using cached assets");
    }

    // Parse volume mounts
    let mounts: Vec<launch::VolumeMount> = args
        .volumes
        .iter()
        .filter_map(|v| launch::parse_volume_mount(v))
        .collect();

    // Parse environment variables
    let env_vars: Vec<(String, String)> = args
        .env
        .iter()
        .filter_map(|e| {
            let parts: Vec<&str> = e.splitn(2, '=').collect();
            if parts.len() == 2 {
                Some((parts[0].to_string(), parts[1].to_string()))
            } else {
                None
            }
        })
        .collect();

    // Handle subcommands
    match args.command {
        Some(Command::Start) => {
            // Start daemon mode
            let config = launch::DaemonConfig {
                cache_dir,
                manifest,
                mounts,
                cpus: args.cpus,
                mem: args.mem,
                debug: args.debug,
            };

            match launch::start_daemon(config) {
                Ok(()) => {
                    println!("Daemon started. Use 'exec' to run commands.");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    ExitCode::from(1)
                }
            }
        }

        Some(Command::Exec { command }) => {
            // Execute in daemon
            let config = launch::ExecConfig {
                cache_dir,
                manifest,
                command: if command.is_empty() {
                    None
                } else {
                    Some(command)
                },
                mounts,
                env_vars,
                workdir: args.workdir,
                debug: args.debug,
            };

            match launch::exec_in_daemon(config) {
                Ok(exit_code) => ExitCode::from(exit_code as u8),
                Err(e) => {
                    eprintln!("error: {}", e);
                    ExitCode::from(1)
                }
            }
        }

        Some(Command::Stop) => {
            // Stop daemon
            match launch::stop_daemon(&cache_dir, args.debug) {
                Ok(()) => {
                    println!("Daemon stopped.");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    ExitCode::from(1)
                }
            }
        }

        Some(Command::Status) => {
            // Check daemon status
            match launch::daemon_status(&cache_dir) {
                Ok(status) => {
                    println!("{}", status);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    ExitCode::from(1)
                }
            }
        }

        None => {
            // Ephemeral mode (original behavior)
            let config = launch::LaunchConfig {
                cache_dir,
                manifest,
                command: if args.run_command.is_empty() {
                    None
                } else {
                    Some(args.run_command)
                },
                mounts,
                env_vars,
                workdir: args.workdir,
                cpus: args.cpus,
                mem: args.mem,
                debug: args.debug,
            };

            match launch::launch_vm(config) {
                Ok(exit_code) => ExitCode::from(exit_code as u8),
                Err(e) => {
                    eprintln!("error: {}", e);
                    ExitCode::from(1)
                }
            }
        }
    }
}

/// Load packed data from the binary.
///
/// On macOS, tries to read from the Mach-O section first (for signed single-file binaries).
/// Falls back to footer-based reading (sidecar or appended assets).
fn load_packed_data(
    exe_path: &std::path::Path,
    debug: bool,
) -> Result<PackedData, Box<dyn std::error::Error>> {
    // On macOS, try section-based reading first
    #[cfg(target_os = "macos")]
    {
        if let Some(embedded) = section::read_embedded_section() {
            if debug {
                eprintln!("debug: found embedded data in Mach-O section");
            }
            return Ok(PackedData::Section {
                manifest: embedded.manifest,
                checksum: embedded.header.checksum,
                assets_ptr: embedded.assets_ptr,
                assets_size: embedded.assets_size,
            });
        }
        if debug {
            eprintln!("debug: no section data, trying footer-based mode");
        }
    }

    // Fall back to footer-based reading
    let footer = read_footer(exe_path)?;
    let manifest = read_manifest(exe_path)?;

    Ok(PackedData::FooterBased { manifest, footer })
}
