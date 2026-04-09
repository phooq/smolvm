//! Shared queues and wake notifications for the virtio-net backend.

use crossbeam_queue::ArrayQueue;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Default queue capacity for guest/host ethernet frames.
pub const DEFAULT_FRAME_QUEUE_CAPACITY: usize = 1024;

/// Shared queues and wake handles for the host-side virtio-net runtime.
pub struct NetworkFrameQueues {
    /// Raw ethernet frames emitted by the guest and waiting for smoltcp.
    pub guest_to_host: ArrayQueue<Vec<u8>>,
    /// Raw ethernet frames emitted by smoltcp and waiting for libkrun.
    pub host_to_guest: ArrayQueue<Vec<u8>>,
    /// Wake the smoltcp poll loop when a guest frame arrives.
    pub guest_wake: WakePipe,
    /// Wake the libkrun writer thread when a host frame is ready.
    pub host_wake: WakePipe,
    /// Wake the smoltcp poll loop when a TCP relay thread has new data.
    pub relay_wake: WakePipe,
    /// Signals that the helper process should shut down.
    shutting_down: AtomicBool,
}

impl NetworkFrameQueues {
    /// Create a new shared queue set wrapped in `Arc`.
    pub fn shared(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            guest_to_host: ArrayQueue::new(capacity),
            host_to_guest: ArrayQueue::new(capacity),
            guest_wake: WakePipe::new(),
            host_wake: WakePipe::new(),
            relay_wake: WakePipe::new(),
            shutting_down: AtomicBool::new(false),
        })
    }

    /// Mark the runtime as shutting down and wake all waiters.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.guest_wake.wake();
        self.host_wake.wake();
        self.relay_wake.wake();
    }

    /// Whether shutdown has been requested.
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }
}

/// Cross-platform wake notification built on `pipe(2)`.
#[derive(Debug)]
pub struct WakePipe {
    read_fd: OwnedFd,
    write_fd: OwnedFd,
}

impl WakePipe {
    /// Create a non-blocking wake pipe.
    pub fn new() -> Self {
        let mut fds = [0i32; 2];

        // SAFETY: `pipe` initializes both file descriptors on success.
        let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(
            result,
            0,
            "pipe() failed: {}",
            std::io::Error::last_os_error()
        );

        // SAFETY: both descriptors are valid after a successful `pipe`.
        unsafe {
            set_nonblock_cloexec(fds[0]);
            set_nonblock_cloexec(fds[1]);
        }

        Self {
            // SAFETY: ownership of the raw file descriptors transfers here.
            read_fd: unsafe { OwnedFd::from_raw_fd(fds[0]) },
            write_fd: unsafe { OwnedFd::from_raw_fd(fds[1]) },
        }
    }

    /// Signal the waiting side.
    pub fn wake(&self) {
        let byte = [1u8; 1];
        // SAFETY: the write end is valid and non-blocking.
        unsafe {
            libc::write(self.write_fd.as_raw_fd(), byte.as_ptr().cast(), byte.len());
        }
    }

    /// Drain all pending wake bytes.
    pub fn drain(&self) {
        let mut buf = [0u8; 256];
        loop {
            // SAFETY: the read end is valid and non-blocking.
            let read =
                unsafe { libc::read(self.read_fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if read <= 0 {
                break;
            }
        }
    }

    /// Wait until the pipe is readable or the timeout elapses.
    pub fn wait(&self, timeout: Option<Duration>) -> std::io::Result<bool> {
        let timeout_ms = timeout
            .map(|duration| duration.as_millis().min(i32::MAX as u128) as i32)
            .unwrap_or(-1);
        let mut pollfd = libc::pollfd {
            fd: self.read_fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };

        // SAFETY: `pollfd` points to a valid descriptor and struct.
        let result = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(result > 0 && pollfd.revents & libc::POLLIN != 0)
    }

    /// File descriptor for `poll(2)`.
    pub fn as_raw_fd(&self) -> RawFd {
        self.read_fd.as_raw_fd()
    }
}

impl Clone for WakePipe {
    fn clone(&self) -> Self {
        let read_fd = self
            .read_fd
            .try_clone()
            .expect("wake pipe read fd should be clonable");
        let write_fd = self
            .write_fd
            .try_clone()
            .expect("wake pipe write fd should be clonable");
        Self { read_fd, write_fd }
    }
}

impl Default for WakePipe {
    fn default() -> Self {
        Self::new()
    }
}

/// Set `O_NONBLOCK` and `FD_CLOEXEC` on a file descriptor.
///
/// # Safety
///
/// `fd` must be a valid open file descriptor.
unsafe fn set_nonblock_cloexec(fd: RawFd) {
    // SAFETY: caller guarantees `fd` is valid.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(
        flags >= 0,
        "fcntl(F_GETFL) failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: caller guarantees `fd` is valid.
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert!(
        result >= 0,
        "fcntl(F_SETFL) failed: {}",
        std::io::Error::last_os_error()
    );

    // SAFETY: caller guarantees `fd` is valid.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(
        flags >= 0,
        "fcntl(F_GETFD) failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: caller guarantees `fd` is valid.
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    assert!(
        result >= 0,
        "fcntl(F_SETFD) failed: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_pipe_round_trip() {
        let pipe = WakePipe::new();
        pipe.wake();
        assert!(pipe.wait(Some(Duration::from_millis(10))).unwrap());
        pipe.drain();
        assert!(!pipe.wait(Some(Duration::from_millis(1))).unwrap());
    }

    #[test]
    fn queues_are_fifo() {
        let queues = NetworkFrameQueues::shared(4);
        queues.guest_to_host.push(vec![1, 2, 3]).unwrap();
        queues.guest_to_host.push(vec![4, 5, 6]).unwrap();

        assert_eq!(queues.guest_to_host.pop(), Some(vec![1, 2, 3]));
        assert_eq!(queues.guest_to_host.pop(), Some(vec![4, 5, 6]));
        assert_eq!(queues.guest_to_host.pop(), None);
    }
}
