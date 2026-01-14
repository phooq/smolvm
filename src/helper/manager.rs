//! Helper VM lifecycle management.
//!
//! The HelperManager is responsible for starting and stopping the helper VM,
//! which runs the helper daemon for OCI image management.

use crate::error::{Error, Result};
use crate::storage::StorageDisk;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::launcher::launch_helper_vm;

/// State of the helper VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperState {
    /// Helper is not running.
    Stopped,
    /// Helper is starting up.
    Starting,
    /// Helper is running and ready.
    Running,
    /// Helper is shutting down.
    Stopping,
}

/// Internal state shared between threads.
struct HelperInner {
    state: HelperState,
    /// Child process PID (if running).
    child_pid: Option<libc::pid_t>,
}

/// Helper VM manager.
///
/// Manages the lifecycle of the helper VM which handles OCI image operations.
pub struct HelperManager {
    /// Path to the helper rootfs.
    rootfs_path: PathBuf,
    /// Storage disk for OCI layers.
    storage_disk: StorageDisk,
    /// vsock socket path for control channel.
    vsock_socket: PathBuf,
    /// Console log path (optional).
    console_log: Option<PathBuf>,
    /// Internal state.
    inner: Arc<Mutex<HelperInner>>,
}

impl HelperManager {
    /// Create a new helper manager.
    ///
    /// # Arguments
    ///
    /// * `rootfs_path` - Path to the helper VM rootfs
    /// * `storage_disk` - Storage disk for OCI layers
    pub fn new(rootfs_path: impl Into<PathBuf>, storage_disk: StorageDisk) -> Result<Self> {
        let rootfs_path = rootfs_path.into();

        // Create runtime directory for sockets
        let runtime_dir = dirs::runtime_dir()
            .or_else(|| dirs::cache_dir())
            .unwrap_or_else(|| PathBuf::from("/tmp"));

        let smolvm_runtime = runtime_dir.join("smolvm");
        std::fs::create_dir_all(&smolvm_runtime)?;

        let vsock_socket = smolvm_runtime.join("helper.sock");

        // Console log path
        let console_log = Some(smolvm_runtime.join("helper-console.log"));

        Ok(Self {
            rootfs_path,
            storage_disk,
            vsock_socket,
            console_log,
            inner: Arc::new(Mutex::new(HelperInner {
                state: HelperState::Stopped,
                child_pid: None,
            })),
        })
    }

    /// Get the default helper manager.
    ///
    /// Uses default paths for rootfs and storage.
    pub fn default() -> Result<Self> {
        let rootfs_path = Self::default_rootfs_path()?;
        let storage_disk = StorageDisk::open_or_create()?;

        Self::new(rootfs_path, storage_disk)
    }

    /// Get the default path for the helper rootfs.
    pub fn default_rootfs_path() -> Result<PathBuf> {
        let data_dir = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| Error::Storage("could not determine data directory".into()))?;

        Ok(data_dir.join("smolvm").join("helper-rootfs"))
    }

    /// Get the current state of the helper.
    pub fn state(&self) -> HelperState {
        self.inner.lock().unwrap().state
    }

    /// Check if the helper is running.
    pub fn is_running(&self) -> bool {
        self.state() == HelperState::Running
    }

    /// Get the vsock socket path.
    pub fn vsock_socket(&self) -> &Path {
        &self.vsock_socket
    }

    /// Get the console log path.
    pub fn console_log(&self) -> Option<&Path> {
        self.console_log.as_deref()
    }

    /// Ensure the helper is running.
    ///
    /// If the helper is not running, this starts it.
    /// If the helper is already running, this is a no-op.
    pub fn ensure_running(&self) -> Result<()> {
        let state = self.state();

        match state {
            HelperState::Running => Ok(()),
            HelperState::Starting => self.wait_for_ready(),
            HelperState::Stopped => self.start(),
            HelperState::Stopping => {
                self.wait_for_stop()?;
                self.start()
            }
        }
    }

    /// Start the helper VM.
    pub fn start(&self) -> Result<()> {
        // Check and update state
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.state != HelperState::Stopped {
                return Err(Error::HelperError(
                    "helper already starting or running".into(),
                ));
            }
            inner.state = HelperState::Starting;
        }

        tracing::info!(
            rootfs = %self.rootfs_path.display(),
            storage = %self.storage_disk.path().display(),
            socket = %self.vsock_socket.display(),
            "starting helper VM"
        );

        // Validate rootfs exists
        if !self.rootfs_path.exists() {
            let mut inner = self.inner.lock().unwrap();
            inner.state = HelperState::Stopped;
            return Err(Error::HelperError(format!(
                "helper rootfs not found: {}",
                self.rootfs_path.display()
            )));
        }

        // Clean up old socket
        let _ = std::fs::remove_file(&self.vsock_socket);

        // Fork child process
        let pid = unsafe { libc::fork() };

        match pid {
            -1 => {
                // Fork failed
                let err = std::io::Error::last_os_error();
                let mut inner = self.inner.lock().unwrap();
                inner.state = HelperState::Stopped;
                Err(Error::HelperError(format!("fork failed: {}", err)))
            }
            0 => {
                // Child process - launch the VM
                // This should never return on success

                // Close stdin to detach from terminal
                unsafe {
                    libc::close(0);
                }

                // Create new session
                unsafe {
                    libc::setsid();
                }

                // Launch the helper VM
                let result = launch_helper_vm(
                    &self.rootfs_path,
                    &self.storage_disk,
                    &self.vsock_socket,
                    self.console_log.as_deref(),
                );

                // If we get here, something went wrong
                if let Err(e) = result {
                    eprintln!("helper VM failed to start: {}", e);
                }

                // Exit child process
                unsafe {
                    libc::_exit(1);
                }
            }
            child_pid => {
                // Parent process
                tracing::debug!(pid = child_pid, "forked helper VM process");

                // Store child PID
                {
                    let mut inner = self.inner.lock().unwrap();
                    inner.child_pid = Some(child_pid);
                }

                // Wait for the helper to be ready
                match self.wait_for_ready() {
                    Ok(_) => {
                        let mut inner = self.inner.lock().unwrap();
                        inner.state = HelperState::Running;
                        tracing::info!(pid = child_pid, "helper VM is ready");
                        Ok(())
                    }
                    Err(e) => {
                        // Kill child if startup failed
                        unsafe {
                            libc::kill(child_pid, libc::SIGTERM);
                        }
                        let mut inner = self.inner.lock().unwrap();
                        inner.state = HelperState::Stopped;
                        inner.child_pid = None;
                        Err(e)
                    }
                }
            }
        }
    }

    /// Stop the helper VM.
    pub fn stop(&self) -> Result<()> {
        let (state, child_pid) = {
            let inner = self.inner.lock().unwrap();
            (inner.state, inner.child_pid)
        };

        if state == HelperState::Stopped {
            return Ok(());
        }

        {
            let mut inner = self.inner.lock().unwrap();
            inner.state = HelperState::Stopping;
        }

        tracing::info!("stopping helper VM");

        // Try graceful shutdown via vsock first
        if let Ok(mut client) = super::HelperClient::connect(&self.vsock_socket) {
            let _ = client.shutdown();
            // Give it a moment to shut down
            std::thread::sleep(Duration::from_millis(500));
        }

        // If there's a child process, signal it
        if let Some(pid) = child_pid {
            // First try SIGTERM
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }

            // Wait briefly for graceful shutdown
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                let mut status: libc::c_int = 0;
                let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };

                if result == pid {
                    // Process exited
                    break;
                } else if result < 0 {
                    // Error
                    break;
                }

                std::thread::sleep(Duration::from_millis(100));
            }

            // Force kill if still running
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
        }

        // Clean up
        {
            let mut inner = self.inner.lock().unwrap();
            inner.state = HelperState::Stopped;
            inner.child_pid = None;
        }

        // Remove socket
        let _ = std::fs::remove_file(&self.vsock_socket);

        Ok(())
    }

    /// Wait for the helper to be ready.
    fn wait_for_ready(&self) -> Result<()> {
        let timeout = Duration::from_secs(30);
        let start = Instant::now();
        let poll_interval = Duration::from_millis(100);

        tracing::debug!("waiting for helper to be ready");

        while start.elapsed() < timeout {
            // Check if child process is still alive
            {
                let inner = self.inner.lock().unwrap();
                if let Some(pid) = inner.child_pid {
                    let mut status: libc::c_int = 0;
                    let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };

                    if result == pid {
                        // Child exited
                        return Err(Error::HelperError(
                            "helper process exited during startup".into(),
                        ));
                    }
                }
            }

            // Try to connect to vsock socket
            if self.vsock_socket.exists() {
                match UnixStream::connect(&self.vsock_socket) {
                    Ok(stream) => {
                        drop(stream);

                        // Try to ping
                        match super::HelperClient::connect(&self.vsock_socket) {
                            Ok(mut client) => {
                                if client.ping().is_ok() {
                                    tracing::debug!("helper ping successful");
                                    return Ok(());
                                }
                            }
                            Err(e) => {
                                tracing::trace!("ping failed: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::trace!("connect failed: {}", e);
                    }
                }
            }

            std::thread::sleep(poll_interval);
        }

        Err(Error::HelperError(format!(
            "helper did not become ready within {} seconds",
            timeout.as_secs()
        )))
    }

    /// Wait for the helper to stop.
    fn wait_for_stop(&self) -> Result<()> {
        let timeout = Duration::from_secs(10);
        let start = Instant::now();

        while start.elapsed() < timeout {
            if self.state() == HelperState::Stopped {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        Err(Error::HelperError("timeout waiting for helper to stop".into()))
    }

    /// Check if helper process is still running.
    pub fn check_alive(&self) -> bool {
        let inner = self.inner.lock().unwrap();

        if let Some(pid) = inner.child_pid {
            let mut status: libc::c_int = 0;
            let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };

            // If result is 0, process is still running
            // If result is pid, process has exited
            // If result is -1, error (process doesn't exist)
            result == 0
        } else {
            false
        }
    }
}

impl Drop for HelperManager {
    fn drop(&mut self) {
        // Best-effort cleanup
        let _ = self.stop();
    }
}
