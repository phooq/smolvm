//! Test disk-based root filesystem with libkrun.
//!
//! Usage: DYLD_LIBRARY_PATH=/opt/homebrew/lib cargo run --bin test_disk_root

use std::ffi::CString;
use std::ptr;

#[link(name = "krun")]

extern "C" {
    fn krun_set_log_level(level: u32) -> i32;
    fn krun_create_ctx() -> i32;
    fn krun_free_ctx(ctx: u32);
    fn krun_set_vm_config(ctx: u32, num_vcpus: u8, ram_mib: u32) -> i32;
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
    fn krun_set_root_disk_remount(
        ctx: u32,
        device: *const libc::c_char,
        fstype: *const libc::c_char,
        options: *const libc::c_char,
    ) -> i32;

    // Deprecated but simpler - just sets disk as root directly
    fn krun_set_root_disk(ctx: u32, disk_path: *const libc::c_char) -> i32;
    fn krun_set_root(ctx: u32, root_path: *const libc::c_char) -> i32;
    fn krun_set_console_output(ctx: u32, filepath: *const libc::c_char) -> i32;
    fn krun_set_port_map(ctx: u32, port_map: *const *const libc::c_char) -> i32;
    fn krun_start_enter(ctx: u32) -> i32;
}

fn main() {
    // Raise file descriptor limits
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

    unsafe {
        // Enable debug logging
        krun_set_log_level(5);

        // Create VM context
        let ctx = krun_create_ctx();
        if ctx < 0 {
            eprintln!("Failed to create context");
            return;
        }
        let ctx = ctx as u32;
        eprintln!("Created context: {}", ctx);

        // Set VM config: 1 CPU, 512MB RAM (same as smolvm default)
        if krun_set_vm_config(ctx, 1, 512) < 0 {
            eprintln!("Failed to set VM config");
            krun_free_ctx(ctx);
            return;
        }
        eprintln!("Set VM config");

        // Set virtiofs root filesystem
        let root_path = CString::new("/Users/binsquare/Documents/smolvm/helper-rootfs/rootfs").unwrap();
        let ret = krun_set_root(ctx, root_path.as_ptr());
        eprintln!("krun_set_root returned: {}", ret);
        if ret < 0 {
            eprintln!("Failed to set root");
            krun_free_ctx(ctx);
            return;
        }

        // Set empty port map
        let empty_ports: Vec<*const libc::c_char> = vec![ptr::null()];
        if krun_set_port_map(ctx, empty_ports.as_ptr()) < 0 {
            eprintln!("Failed to set port map");
            krun_free_ctx(ctx);
            return;
        }
        eprintln!("Set port map");

        // Skip console output for now - it may cause issues
        // let console_log = CString::new("/tmp/smolvm-test/console.log").unwrap();
        // let ret = krun_set_console_output(ctx, console_log.as_ptr());
        // eprintln!("krun_set_console_output returned: {}", ret);

        // Skip workdir - it's optional
        // let workdir = CString::new("/").unwrap();
        // krun_set_workdir(ctx, workdir.as_ptr());
        eprintln!("Skipped workdir");

        // Build environment variables like smolvm does
        let env_strings: Vec<CString> = vec![
            CString::new("HOSTNAME=test-vm").unwrap(),
            CString::new("HOME=/root").unwrap(),
            CString::new("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin").unwrap(),
            CString::new("TERM=xterm").unwrap(),
        ];
        let mut envp: Vec<*const libc::c_char> = env_strings.iter().map(|s| s.as_ptr()).collect();
        envp.push(ptr::null());

        // Set exec command - simple echo to test
        let exec_path = CString::new("/bin/echo").unwrap();
        let arg1 = CString::new("VIRTIOFS_ROOT_WORKS").unwrap();
        let argv: Vec<*const libc::c_char> = vec![arg1.as_ptr(), ptr::null()];

        let ret = krun_set_exec(ctx, exec_path.as_ptr(), argv.as_ptr(), envp.as_ptr());
        eprintln!("krun_set_exec returned: {}", ret);
        if ret < 0 {
            eprintln!("Failed to set exec");
            krun_free_ctx(ctx);
            return;
        }

        eprintln!("Starting VM with disk-based root...");

        // Start VM - this should not return on success
        let ret = krun_start_enter(ctx);
        eprintln!("krun_start_enter returned: {} (should not happen on success)", ret);
    }
}
