//! Host-side listeners for published virtio-net ports.
//!
//! Context
//! =======
//!
//! This module is the host-facing half of published `HOST:GUEST[/PROTO]`
//! mappings for the virtio-net backend.
//!
//! The outbound virtio path already handles guest-initiated traffic:
//!
//! ```text
//! guest TCP/UDP -> smoltcp socket -> host socket -> remote server
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
//! ```text
//! host UDP client -> host UdpSocket bound to localhost:HOST
//!                -> poll loop allocates gateway_ip:ephemeral source port
//!                -> smoltcp sends gateway_ip:ephemeral -> guest_ip:GUEST
//!                -> guest reply returns to the same smoltcp socket
//!                -> listener thread sends reply bytes back to the original host peer
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

use crate::data::network::PortMapping;
use crate::network::virtio::queues::WakePipe;
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Maximum number of accepted published sockets queued for the poll loop.
pub const DEFAULT_PUBLISH_QUEUE_CAPACITY: usize = 64;
/// Maximum number of queued UDP datagrams for one published host port.
pub const DEFAULT_PUBLISHED_UDP_QUEUE_CAPACITY: usize = 128;

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

/// Host UDP datagram waiting for the poll loop.
pub struct PublishedUdpDatagram {
    /// Published host port that received the datagram.
    pub host_port: u16,
    /// Guest UDP port that should receive the datagram.
    pub guest_port: u16,
    /// Original host peer on the localhost side of the published port.
    pub peer_addr: SocketAddr,
    /// Datagram payload received from the host peer.
    pub payload: Vec<u8>,
}

/// Guest UDP reply that should be sent back through a published host port.
pub struct PublishedUdpReply {
    /// Host peer that originally sent traffic to the published port.
    pub peer_addr: SocketAddr,
    /// UDP payload that should be written back to the host peer.
    pub payload: Vec<u8>,
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
        let tcp_mappings: Vec<PortMapping> = port_mappings
            .iter()
            .copied()
            .filter(|mapping| !mapping.is_udp())
            .collect();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::with_capacity(tcp_mappings.len());

        for mapping in tcp_mappings {
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

/// Running published UDP listener set for one guest NIC.
pub struct PublishedUdpListeners {
    shutdown: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
    reply_senders: HashMap<u16, SyncSender<PublishedUdpReply>>,
}

impl PublishedUdpListeners {
    /// Start one non-blocking UDP listener thread per published UDP port.
    pub fn start(
        port_mappings: &[PortMapping],
        incoming_tx: SyncSender<PublishedUdpDatagram>,
        publish_wake: WakePipe,
    ) -> io::Result<Self> {
        let udp_mappings: Vec<PortMapping> = port_mappings
            .iter()
            .copied()
            .filter(PortMapping::is_udp)
            .collect();
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::with_capacity(udp_mappings.len());
        let mut reply_senders = HashMap::with_capacity(udp_mappings.len());

        for mapping in udp_mappings {
            let socket = match UdpSocket::bind((Ipv4Addr::LOCALHOST, mapping.host)) {
                Ok(socket) => socket,
                Err(err) => {
                    shutdown.store(true, Ordering::SeqCst);
                    join_listener_threads(&mut handles);
                    return Err(err);
                }
            };
            if let Err(err) = socket.set_nonblocking(true) {
                shutdown.store(true, Ordering::SeqCst);
                join_listener_threads(&mut handles);
                return Err(err);
            }

            let (reply_tx, reply_rx) = mpsc::sync_channel(DEFAULT_PUBLISHED_UDP_QUEUE_CAPACITY);
            reply_senders.insert(mapping.host, reply_tx);

            let incoming_tx = incoming_tx.clone();
            let publish_wake = publish_wake.clone();
            let shutdown_flag = shutdown.clone();
            let host_port = mapping.host;
            let guest_port = mapping.guest;

            let handle = thread::Builder::new()
                .name(format!("smolvm-pub-udp-{}", host_port))
                .spawn(move || {
                    run_published_udp_listener(
                        socket,
                        host_port,
                        guest_port,
                        incoming_tx,
                        reply_rx,
                        publish_wake,
                        shutdown_flag,
                    )
                })
                .map_err(|err| {
                    shutdown.store(true, Ordering::SeqCst);
                    join_listener_threads(&mut handles);
                    io::Error::other(format!(
                        "failed to spawn published UDP listener thread for {host_port}: {err}"
                    ))
                })?;
            handles.push(handle);
        }

        Ok(Self {
            shutdown,
            handles,
            reply_senders,
        })
    }

    /// Look up the reply channel for one published host UDP port.
    pub fn reply_sender(&self, host_port: u16) -> Option<SyncSender<PublishedUdpReply>> {
        self.reply_senders.get(&host_port).cloned()
    }
}

impl Drop for PublishedUdpListeners {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        join_listener_threads(&mut self.handles);
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

fn run_published_udp_listener(
    socket: UdpSocket,
    host_port: u16,
    guest_port: u16,
    incoming_tx: SyncSender<PublishedUdpDatagram>,
    reply_rx: mpsc::Receiver<PublishedUdpReply>,
    publish_wake: WakePipe,
    shutdown: Arc<AtomicBool>,
) {
    let mut read_buffer = [0u8; 16 * 1024];

    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }

        let mut did_work = false;

        loop {
            match socket.recv_from(&mut read_buffer) {
                Ok((bytes_read, peer_addr)) => {
                    let datagram = PublishedUdpDatagram {
                        host_port,
                        guest_port,
                        peer_addr,
                        payload: read_buffer[..bytes_read].to_vec(),
                    };

                    match incoming_tx.try_send(datagram) {
                        Ok(()) => {
                            publish_wake.wake();
                            did_work = true;
                        }
                        Err(TrySendError::Full(datagram)) => {
                            tracing::warn!(
                                host_port = datagram.host_port,
                                guest_port = datagram.guest_port,
                                peer_addr = %datagram.peer_addr,
                                "dropping published UDP datagram because the ingress queue is full"
                            );
                            did_work = true;
                        }
                        Err(TrySendError::Disconnected(_)) => return,
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    tracing::warn!(
                        host_port,
                        guest_port,
                        error = %err,
                        "published UDP listener recv_from failed"
                    );
                    break;
                }
            }
        }

        loop {
            match reply_rx.try_recv() {
                Ok(reply) => {
                    if let Err(err) = socket.send_to(&reply.payload, reply.peer_addr) {
                        tracing::warn!(
                            host_port,
                            guest_port,
                            peer_addr = %reply.peer_addr,
                            error = %err,
                            "published UDP listener send_to failed"
                        );
                    }
                    did_work = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }

        if !did_work {
            thread::sleep(ACCEPT_POLL_INTERVAL);
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

/// Create the bounded channel used to hand published host UDP datagrams to the poll loop.
pub fn published_udp_datagram_channel() -> (
    SyncSender<PublishedUdpDatagram>,
    mpsc::Receiver<PublishedUdpDatagram>,
) {
    mpsc::sync_channel(DEFAULT_PUBLISHED_UDP_QUEUE_CAPACITY)
}
