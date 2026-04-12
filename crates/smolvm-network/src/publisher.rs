//! Host-side TCP listeners for published virtio-net ports.
//!
//! Context
//! =======
//!
//! This module is the host-facing half of `-p HOST:GUEST` for the virtio-net
//! backend.
//!
//! The outbound virtio path already handles guest-initiated TCP:
//!
//! ```text
//! guest TCP connect -> smoltcp socket -> host TcpStream -> remote server
//! ```
//!
//! Published ports invert the initiator:
//!
//! ```text
//! host client -> host TcpListener -> accepted TcpStream
//!           -> smoltcp creates gateway-side TCP connection to guest_ip:GUEST
//!           -> relay thread bridges the accepted host socket to the guest flow
//! ```
//!
//! High-level flow:
//!
//! ```text
//! host client connects to 127.0.0.1:HOST
//!   -> PublishedPortListeners accepts TcpStream
//!   -> AcceptedPublishedConnection sent over a bounded channel
//!   -> relay_wake wakes the smoltcp poll loop
//!   -> poll loop creates a guest-facing TCP socket to guest_ip:GUEST
//!   -> once Established, tcp_relay uses the accepted host TcpStream directly
//! ```
//!
//! Equivalent operational mental model:
//! - `TcpListener::bind(...).accept()` plays the role of a tiny `nc -l` or
//!   `socat TCP-LISTEN:HOST,...`
//! - instead of shelling out to a proxy process, we hand the accepted socket
//!   into the in-process smoltcp gateway so the guest sees a normal TCP
//!   connection on its `eth0` interface.

use crate::queues::WakePipe;
use crate::PortMapping;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Maximum number of accepted published sockets queued for the poll loop.
pub const DEFAULT_PUBLISH_QUEUE_CAPACITY: usize = 64;

/// Accepted host TCP connection waiting for the smoltcp poll loop.
pub struct AcceptedPublishedConnection {
    /// Connected host-side socket returned by `accept(2)`.
    pub stream: TcpStream,
    /// Host port that accepted the connection.
    pub host_port: u16,
    /// Guest port the connection should be forwarded to.
    pub guest_port: u16,
    /// Remote peer that connected to the published port.
    pub peer_addr: SocketAddr,
}

/// Running published-port listener set for one guest NIC.
pub struct PublishedPortListeners {
    shutdown: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl PublishedPortListeners {
    /// Start one non-blocking listener thread per published port.
    pub fn start(
        port_mappings: &[PortMapping],
        accepted_tx: SyncSender<AcceptedPublishedConnection>,
        publish_wake: WakePipe,
    ) -> io::Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::with_capacity(port_mappings.len());

        for mapping in port_mappings {
            let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, mapping.host)) {
                Ok(listener) => listener,
                Err(err) => {
                    shutdown.store(true, Ordering::SeqCst);
                    join_listener_threads(&mut handles);
                    return Err(err);
                }
            };
            if let Err(err) = listener.set_nonblocking(true) {
                shutdown.store(true, Ordering::SeqCst);
                join_listener_threads(&mut handles);
                return Err(err);
            }

            let accepted_tx = accepted_tx.clone();
            let publish_wake = publish_wake.clone();
            let shutdown_flag = shutdown.clone();
            let host_port = mapping.host;
            let guest_port = mapping.guest;

            let handle = thread::Builder::new()
                .name(format!("smolvm-pub-{}", host_port))
                .spawn(move || {
                    run_published_port_listener(
                        listener,
                        host_port,
                        guest_port,
                        accepted_tx,
                        publish_wake,
                        shutdown_flag,
                    )
                })
                .map_err(|err| {
                    shutdown.store(true, Ordering::SeqCst);
                    join_listener_threads(&mut handles);
                    io::Error::other(format!(
                        "failed to spawn published-port listener thread for {host_port}: {err}"
                    ))
                })?;
            handles.push(handle);
        }

        Ok(Self { shutdown, handles })
    }
}

impl Drop for PublishedPortListeners {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        join_listener_threads(&mut self.handles);
    }
}

fn join_listener_threads(handles: &mut Vec<JoinHandle<()>>) {
    for handle in handles.drain(..) {
        let _ = handle.join();
    }
}

fn run_published_port_listener(
    listener: TcpListener,
    host_port: u16,
    guest_port: u16,
    accepted_tx: SyncSender<AcceptedPublishedConnection>,
    publish_wake: WakePipe,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }

        match listener.accept() {
            Ok((stream, peer_addr)) => {
                let accepted = AcceptedPublishedConnection {
                    stream,
                    host_port,
                    guest_port,
                    peer_addr,
                };

                match accepted_tx.try_send(accepted) {
                    Ok(()) => publish_wake.wake(),
                    Err(TrySendError::Full(accepted)) => {
                        tracing::warn!(
                            host_port = accepted.host_port,
                            guest_port = accepted.guest_port,
                            peer_addr = %accepted.peer_addr,
                            "dropping published TCP connection because the accept queue is full"
                        );
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(err) => {
                tracing::warn!(
                    host_port,
                    guest_port,
                    error = %err,
                    "published port listener accept failed"
                );
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }
}

/// Create the bounded channel used to hand accepted host sockets to the poll loop.
pub fn accepted_connection_channel() -> (
    SyncSender<AcceptedPublishedConnection>,
    mpsc::Receiver<AcceptedPublishedConnection>,
) {
    mpsc::sync_channel(DEFAULT_PUBLISH_QUEUE_CAPACITY)
}
