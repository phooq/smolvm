//! Helper VM launcher.
//!
//! This module provides the low-level VM launching functionality
//! that runs in the forked child process.

use crate::error::{Error, Result};
use crate::protocol::ports;
use crate::storage::StorageDisk;
use std::ffi::CString;
use std::path::Path;

use super::{HELPER_CPUS, HELPER_MEMORY_MIB};

// FFI bindings to libkrun (duplicated here to avoid circular deps)
extern "C" {
    fn krun_set_log_level(level: u32) -> i32;
    fn krun_create_ctx() -> i32;
    fn krun_free_ctx(ctx: u32);
    fn krun_set_vm_config(ctx: u32, num_vcpus: u8, ram_mib: u32) -> i32;
    fn krun_set_root(ctx: u32, root_path: *const libc::c_char) -> i32;
    fn krun_set_workdir(ctx: u32, workdir: *const libc::c_char) -> i32;
    fn krun_set_exec(
        ctx: u32,
        exec_path: *const libc::c_char,
        argv: *const *const libc::c_char,
        envp: *const *const libc::c_char,
    ) -> i32;
    fn krun_add_disk2(
        ctx: u32,
        block_id: *const libc::c_char,
        disk_path: *const libc::c_char,
        disk_format: u32,
        read_only: bool,
    ) -> i32;
    fn krun_add_vsock_port2(
        ctx: u32,
        port: u32,
        filepath: *const libc::c_char,
        listen: bool,
    ) -> i32;
    fn krun_set_console_output(ctx: u32, filepath: *const libc::c_char) -> i32;
    fn krun_set_port_map(ctx: u32, port_map: *const *const libc::c_char) -> i32;
    fn krun_start_enter(ctx: u32) -> i32;
}

/// Launch the helper VM.
///
/// This function is called in the forked child process.
/// It configures and starts the VM using libkrun, which replaces the process.
///
/// # Safety
///
/// This function should only be called after fork() in the child process.
/// It will never return on success (krun_start_enter replaces the process).
pub fn launch_helper_vm(
    rootfs_path: &Path,
    storage_disk: &StorageDisk,
    vsock_socket: &Path,
    console_log: Option<&Path>,
) -> Result<()> {
    // Raise file descriptor limits
    raise_fd_limits();

    unsafe {
        // Set log level (0 = off, increase for debugging)
        krun_set_log_level(0);

        // Create VM context
        let ctx = krun_create_ctx();
        if ctx < 0 {
            return Err(Error::HelperError("failed to create libkrun context".into()));
        }
        let ctx = ctx as u32;

        // Set VM config
        if krun_set_vm_config(ctx, HELPER_CPUS, HELPER_MEMORY_MIB) < 0 {
            krun_free_ctx(ctx);
            return Err(Error::HelperError("failed to set VM config".into()));
        }

        // Set root filesystem
        let root = path_to_cstring(rootfs_path)?;
        if krun_set_root(ctx, root.as_ptr()) < 0 {
            krun_free_ctx(ctx);
            return Err(Error::HelperError("failed to set root filesystem".into()));
        }

        // Set empty port map (required by libkrun)
        let empty_ports: Vec<*const libc::c_char> = vec![std::ptr::null()];
        if krun_set_port_map(ctx, empty_ports.as_ptr()) < 0 {
            krun_free_ctx(ctx);
            return Err(Error::HelperError("failed to set port map".into()));
        }

        // Add storage disk
        let block_id = CString::new("storage").unwrap();
        let disk_path = path_to_cstring(storage_disk.path())?;
        if krun_add_disk2(ctx, block_id.as_ptr(), disk_path.as_ptr(), 0, false) < 0 {
            tracing::warn!("failed to add storage disk");
        }

        // Add vsock port for control channel (host listens)
        let socket_path = path_to_cstring(vsock_socket)?;
        if krun_add_vsock_port2(ctx, ports::HELPER_CONTROL, socket_path.as_ptr(), true) < 0 {
            tracing::warn!("failed to add vsock port");
        }

        // Set console output if specified
        if let Some(log_path) = console_log {
            let console_path = path_to_cstring(log_path)?;
            if krun_set_console_output(ctx, console_path.as_ptr()) < 0 {
                tracing::warn!("failed to set console output");
            }
        }

        // Set working directory
        let workdir = CString::new("/").unwrap();
        krun_set_workdir(ctx, workdir.as_ptr());

        // Build environment
        let env_strings = vec![
            CString::new("HOME=/root").unwrap(),
            CString::new("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin").unwrap(),
            CString::new("TERM=xterm-256color").unwrap(),
        ];
        let mut envp: Vec<*const libc::c_char> = env_strings.iter().map(|s| s.as_ptr()).collect();
        envp.push(std::ptr::null());

        // Set exec command (/sbin/init)
        let exec_path = CString::new("/sbin/init").unwrap();
        let argv_strings = vec![CString::new("/sbin/init").unwrap()];
        let mut argv: Vec<*const libc::c_char> = argv_strings.iter().map(|s| s.as_ptr()).collect();
        argv.push(std::ptr::null());

        if krun_set_exec(ctx, exec_path.as_ptr(), argv.as_ptr(), envp.as_ptr()) < 0 {
            krun_free_ctx(ctx);
            return Err(Error::HelperError("failed to set exec command".into()));
        }

        // Start VM (this replaces the process on success)
        tracing::info!("starting helper VM");
        let ret = krun_start_enter(ctx);

        // If we get here, something went wrong
        Err(Error::HelperError(format!(
            "krun_start_enter returned: {}",
            ret
        )))
    }
}

/// Convert a Path to a CString.
fn path_to_cstring(path: &Path) -> Result<CString> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| Error::HelperError("path contains null byte".into()))
}

/// Raise file descriptor limits (required by libkrun).
fn raise_fd_limits() {
    unsafe {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };

        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            limit.rlim_cur = limit.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}
