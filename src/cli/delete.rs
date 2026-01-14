//! Delete command implementation.

use clap::Args;
use smolvm::config::SmolvmConfig;
use smolvm::error::Error;
use smolvm::rootfs::buildah;

/// Delete a VM.
#[derive(Args, Debug)]
pub struct DeleteCmd {
    /// VM name to delete.
    pub name: String,

    /// Force deletion without confirmation.
    #[arg(short, long)]
    pub force: bool,
}

impl DeleteCmd {
    /// Execute the delete command.
    pub fn run(&self, config: &mut SmolvmConfig) -> smolvm::Result<()> {
        // Check if VM exists
        let record = match config.get_vm(&self.name) {
            Some(r) => r.clone(),
            None => {
                return Err(Error::VmNotFound(self.name.clone()));
            }
        };

        // Confirm deletion unless --force
        if !self.force {
            eprint!("Delete VM '{}'? [y/N] ", self.name);
            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_ok() {
                let input = input.trim().to_lowercase();
                if input != "y" && input != "yes" {
                    println!("Cancelled");
                    return Ok(());
                }
            } else {
                println!("Cancelled");
                return Ok(());
            }
        }

        // Clean up buildah container if present
        if let Some(ref cid) = record.container_id {
            tracing::debug!(container_id = %cid, "removing buildah container");

            // Unmount first
            if let Err(e) = buildah::unmount_container(cid) {
                tracing::warn!(error = %e, "failed to unmount container");
            }

            // Remove container
            if let Err(e) = buildah::remove_container(cid) {
                tracing::warn!(error = %e, "failed to remove container");
                eprintln!("Warning: failed to remove buildah container: {}", e);
            }
        }

        // Remove from config
        config.remove_vm(&self.name);
        println!("Deleted VM '{}'", self.name);

        Ok(())
    }
}
