//! List command implementation.

use clap::Args;
use smolvm::config::SmolvmConfig;

/// List all VMs.
#[derive(Args, Debug)]
pub struct ListCmd {
    /// Show detailed output.
    #[arg(short, long)]
    pub verbose: bool,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

impl ListCmd {
    /// Execute the list command.
    pub fn run(&self, config: &SmolvmConfig) -> smolvm::Result<()> {
        let vms: Vec<_> = config.list_vms().collect();

        if vms.is_empty() {
            if !self.json {
                println!("No VMs found");
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
                        "cpus": record.cpus,
                        "memory_mib": record.mem,
                        "pid": record.pid,
                        "rootfs": record.rootfs_source,
                        "container_id": record.container_id,
                        "created_at": record.created_at,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json_vms).unwrap());
        } else {
            // Table output
            println!(
                "{:<20} {:<10} {:<6} {:<10} {:<8} {:<20}",
                "NAME", "STATE", "CPUS", "MEMORY", "PID", "ROOTFS"
            );
            println!("{}", "-".repeat(80));

            for (name, record) in vms {
                let actual_state = record.actual_state();
                let rootfs_display = if record.rootfs_source.len() > 18 {
                    format!("{}...", &record.rootfs_source[..15])
                } else {
                    record.rootfs_source.clone()
                };
                let pid_display = record
                    .pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".to_string());

                println!(
                    "{:<20} {:<10} {:<6} {:<10} {:<8} {:<20}",
                    truncate(name, 18),
                    actual_state,
                    record.cpus,
                    format!("{} MiB", record.mem),
                    pid_display,
                    rootfs_display,
                );

                if self.verbose {
                    if let Some(cid) = &record.container_id {
                        println!("  Container: {}", cid);
                    }
                    if let Some(pf) = &record.pid_file {
                        println!("  PID file: {}", pf);
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
