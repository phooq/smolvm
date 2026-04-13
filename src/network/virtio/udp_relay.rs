//! UDP relay support for the virtio-net backend.
//!
//! Context
//! =======
//!
//! The virtio poll loop already uses smoltcp as the guest-visible gateway. For
//! TCP, that gateway terminates the guest-facing connection in userspace and
//! bridges it to a host `TcpStream`. This file does the UDP equivalent.
//!
//! Important difference from TCP:
//! - UDP is connectionless, so there is no handshake to tell us when a flow is
//!   "open"
//! - the poll loop therefore creates relay state lazily when it first sees a
//!   guest datagram for a `(guest source, destination)` pair
//! - idle flows are expired after a timeout instead of closing through FIN/RST
//!
//! High-level model
//! ----------------
//!
//! ```text
//! guest UDP datagram
//!   -> smoltcp UDP socket bound to the remote destination endpoint
//!   -> UdpRelayTable
//!   -> connected host UdpSocket
//!   -> remote server
//!   <- connected host UdpSocket
//!   <- UdpRelayTable
//!   <- same smoltcp UDP socket
//!   <- guest sees a reply from the original remote IP:port
//! ```
//!
//! Two layers of state are tracked:
//!
//! ```text
//! destination socket table
//!   key: destination IP:port
//!   value: smoltcp UDP socket bound to that endpoint
//!
//! flow table
//!   key: (guest source IP:port, destination IP:port)
//!   value: host UdpSocket relay thread + channels
//! ```
//!
//! Why there are two tables:
//! - smoltcp UDP sockets match incoming packets by destination IP:port
//! - host UDP relay state needs the full guest-source/destination tuple
//! - one destination socket can service many guest source ports
//! - each guest flow still needs its own connected host `UdpSocket`
//!
//! There is no single shell command equivalent for this file. The closest
//! analogy is a small userspace UDP proxy keyed by guest flow.

use crate::network::virtio::queues::WakePipe;
use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::udp::{
    PacketBuffer, PacketMetadata, SendError, Socket as UdpSocket, UdpMetadata,
};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket as HostUdpSocket};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const UDP_PACKET_SLOTS: usize = 16;
const UDP_BUFFER_BYTES: usize = 16 * 1024;
const MAX_DESTINATION_SOCKETS: usize = 256;
const MAX_FLOWS: usize = 512;
const CHANNEL_CAPACITY: usize = 64;
const RELAY_BUFFER_BYTES: usize = 16 * 1024;
const FLOW_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const RELAY_IDLE_SLEEP: Duration = Duration::from_millis(10);

/// Summary counters for host-side UDP relay activity.
#[derive(Debug, Clone, Copy, Default)]
pub struct UdpRelayStats {
    /// Guest UDP datagrams successfully handed off to a host relay thread.
    pub guest_datagrams_forwarded: u64,
    /// Host UDP datagrams successfully written back into smoltcp for the guest.
    pub host_datagrams_forwarded: u64,
    /// Guest UDP datagrams dropped before they could reach a host relay thread.
    pub guest_datagrams_dropped: u64,
    /// Host UDP datagrams dropped before they could be written back to the guest.
    pub host_datagrams_dropped: u64,
}

/// Track active UDP relay state for the virtio poll loop.
pub struct UdpRelayTable {
    destination_sockets: HashMap<SocketAddr, TrackedDestinationSocket>,
    flows: HashMap<(SocketAddr, SocketAddr), TrackedUdpFlow>,
    max_destination_sockets: usize,
    max_flows: usize,
    stats: UdpRelayStats,
}

#[derive(Debug)]
struct TrackedDestinationSocket {
    handle: SocketHandle,
    last_activity: Instant,
}

#[derive(Debug)]
struct TrackedUdpFlow {
    guest_source: SocketAddr,
    destination: SocketAddr,
    to_host: SyncSender<Vec<u8>>,
    from_host: Receiver<Vec<u8>>,
    pending_host_payloads: VecDeque<Vec<u8>>,
    last_activity: Instant,
}

impl UdpRelayTable {
    /// Create a new UDP relay table with bounded socket and flow counts.
    pub fn new(max_destination_sockets: Option<usize>, max_flows: Option<usize>) -> Self {
        Self {
            destination_sockets: HashMap::new(),
            flows: HashMap::new(),
            max_destination_sockets: max_destination_sockets.unwrap_or(MAX_DESTINATION_SOCKETS),
            max_flows: max_flows.unwrap_or(MAX_FLOWS),
            stats: UdpRelayStats::default(),
        }
    }

    /// Ensure a smoltcp UDP socket exists for one remote destination endpoint.
    ///
    /// Why the socket is bound to the *destination* endpoint:
    /// - guest outgoing UDP packets are addressed to the remote IP:port
    /// - smoltcp accepts UDP packets based on the packet's destination port and
    ///   optional destination address
    /// - binding the socket to the remote endpoint lets the gateway "catch"
    ///   those outbound guest datagrams before it forwards them to a host
    ///   socket
    pub fn ensure_socket(&mut self, destination: SocketAddr, sockets: &mut SocketSet<'_>) -> bool {
        if self.destination_sockets.contains_key(&destination) {
            return true;
        }

        let SocketAddr::V4(destination_v4) = destination else {
            tracing::warn!(
                destination = %destination,
                "dropping guest UDP datagram because only IPv4 virtio UDP relay is currently supported"
            );
            return false;
        };

        if self.destination_sockets.len() >= self.max_destination_sockets {
            tracing::warn!(
                destination = %destination,
                "dropping guest UDP datagram because the destination socket table is full"
            );
            return false;
        }

        let rx_meta = vec![PacketMetadata::EMPTY; UDP_PACKET_SLOTS];
        let tx_meta = vec![PacketMetadata::EMPTY; UDP_PACKET_SLOTS];
        let rx_buffer = PacketBuffer::new(rx_meta, vec![0u8; UDP_BUFFER_BYTES]);
        let tx_buffer = PacketBuffer::new(tx_meta, vec![0u8; UDP_BUFFER_BYTES]);
        let mut socket = UdpSocket::new(rx_buffer, tx_buffer);
        if socket
            .bind(IpListenEndpoint {
                addr: Some(IpAddress::Ipv4(*destination_v4.ip())),
                port: destination_v4.port(),
            })
            .is_err()
        {
            tracing::warn!(
                destination = %destination,
                "dropping guest UDP datagram because the smoltcp destination socket could not be created"
            );
            return false;
        }

        let handle = sockets.add(socket);
        self.destination_sockets.insert(
            destination,
            TrackedDestinationSocket {
                handle,
                last_activity: Instant::now(),
            },
        );
        tracing::debug!(destination = %destination, "created virtio UDP destination socket");
        true
    }

    /// Drain guest UDP datagrams from smoltcp sockets and forward them to host relay threads.
    pub fn relay_guest_datagrams(
        &mut self,
        sockets: &mut SocketSet<'_>,
        relay_wake: &Arc<WakePipe>,
    ) {
        let destinations: Vec<SocketAddr> = self.destination_sockets.keys().copied().collect();

        for destination in destinations {
            let Some(handle) = self
                .destination_sockets
                .get(&destination)
                .map(|entry| entry.handle)
            else {
                continue;
            };

            loop {
                let received = {
                    let socket = sockets.get_mut::<UdpSocket>(handle);
                    match socket.recv() {
                        Ok((payload, metadata)) => Some((payload.to_vec(), metadata)),
                        Err(_) => None,
                    }
                };

                let Some((payload, metadata)) = received else {
                    break;
                };

                let Some(guest_source) = socket_addr_from_ip_endpoint(metadata.endpoint) else {
                    self.stats.guest_datagrams_dropped += 1;
                    tracing::warn!(
                        destination = %destination,
                        "dropping guest UDP datagram because the guest source endpoint is not IPv4"
                    );
                    continue;
                };

                if let Some(entry) = self.destination_sockets.get_mut(&destination) {
                    entry.last_activity = Instant::now();
                }

                if self.enqueue_guest_payload(guest_source, destination, payload, relay_wake) {
                    self.stats.guest_datagrams_forwarded += 1;
                } else {
                    self.stats.guest_datagrams_dropped += 1;
                }
            }
        }
    }

    /// Drain host UDP responses back into smoltcp so they can be emitted to the guest.
    pub fn relay_host_datagrams(&mut self, sockets: &mut SocketSet<'_>) {
        let flow_keys: Vec<(SocketAddr, SocketAddr)> = self.flows.keys().copied().collect();
        let mut flows_to_remove = Vec::new();

        for flow_key in flow_keys {
            let Some(destination_handle) = self
                .destination_sockets
                .get(&flow_key.1)
                .map(|entry| entry.handle)
            else {
                flows_to_remove.push(flow_key);
                continue;
            };

            let mut disconnected = false;

            if let Some(flow) = self.flows.get_mut(&flow_key) {
                while let Some(payload) = flow.pending_host_payloads.pop_front() {
                    if send_host_payload(
                        sockets,
                        destination_handle,
                        flow.destination,
                        flow.guest_source,
                        &payload,
                    )
                    .is_ok()
                    {
                        flow.last_activity = Instant::now();
                        self.stats.host_datagrams_forwarded += 1;
                    } else {
                        flow.pending_host_payloads.push_front(payload);
                        break;
                    }
                }

                loop {
                    match flow.from_host.try_recv() {
                        Ok(payload) => {
                            if send_host_payload(
                                sockets,
                                destination_handle,
                                flow.destination,
                                flow.guest_source,
                                &payload,
                            )
                            .is_ok()
                            {
                                flow.last_activity = Instant::now();
                                self.stats.host_datagrams_forwarded += 1;
                            } else {
                                tracing::debug!(
                                    guest_source = %flow.guest_source,
                                    destination = %flow.destination,
                                    "buffering host UDP datagram because the guest UDP socket cannot send yet"
                                );
                                flow.pending_host_payloads.push_back(payload);
                                break;
                            }
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }

                if disconnected && flow.pending_host_payloads.is_empty() {
                    flows_to_remove.push(flow_key);
                }
            }
        }

        for flow_key in flows_to_remove {
            if let Some(flow) = self.flows.remove(&flow_key) {
                tracing::debug!(
                    guest_source = %flow.guest_source,
                    destination = %flow.destination,
                    "removed virtio UDP flow"
                );
            }
        }
    }

    /// Expire idle UDP flows and remove destination sockets no longer in use.
    pub fn cleanup_idle(&mut self, sockets: &mut SocketSet<'_>) {
        let now = Instant::now();

        self.flows.retain(|_, flow| {
            let keep = now.duration_since(flow.last_activity) < FLOW_IDLE_TIMEOUT;
            if !keep {
                tracing::debug!(
                    guest_source = %flow.guest_source,
                    destination = %flow.destination,
                    "expiring idle virtio UDP flow"
                );
            }
            keep
        });

        let destinations_to_remove: Vec<SocketAddr> = self
            .destination_sockets
            .iter()
            .filter_map(|(destination, entry)| {
                let has_flow = self
                    .flows
                    .keys()
                    .any(|(_, flow_destination)| flow_destination == destination);
                let idle = now.duration_since(entry.last_activity) >= FLOW_IDLE_TIMEOUT;
                if !has_flow && idle {
                    Some(*destination)
                } else {
                    None
                }
            })
            .collect();

        for destination in destinations_to_remove {
            if let Some(entry) = self.destination_sockets.remove(&destination) {
                sockets.remove(entry.handle);
                tracing::debug!(destination = %destination, "removed idle virtio UDP destination socket");
            }
        }
    }

    /// Number of active guest UDP flow entries currently tracked by the poll loop.
    pub fn active_flow_count(&self) -> usize {
        self.flows.len()
    }

    /// Number of active smoltcp destination sockets used for UDP interception.
    pub fn active_socket_count(&self) -> usize {
        self.destination_sockets.len()
    }

    /// Snapshot of accumulated UDP relay counters.
    pub fn stats(&self) -> UdpRelayStats {
        self.stats
    }

    fn enqueue_guest_payload(
        &mut self,
        guest_source: SocketAddr,
        destination: SocketAddr,
        payload: Vec<u8>,
        relay_wake: &Arc<WakePipe>,
    ) -> bool {
        let flow_key = (guest_source, destination);
        if !self.flows.contains_key(&flow_key) {
            if self.flows.len() >= self.max_flows {
                tracing::warn!(
                    guest_source = %guest_source,
                    destination = %destination,
                    "dropping guest UDP datagram because the relay flow table is full"
                );
                return false;
            }
            let Some(flow) = create_udp_flow(guest_source, destination, relay_wake.clone()) else {
                tracing::warn!(
                    guest_source = %guest_source,
                    destination = %destination,
                    "dropping guest UDP datagram because the relay flow could not be created"
                );
                return false;
            };
            self.flows.insert(flow_key, flow);
        }

        if let Some(flow) = self.flows.get_mut(&flow_key) {
            flow.last_activity = Instant::now();
            match flow.to_host.try_send(payload) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    tracing::warn!(
                        guest_source = %guest_source,
                        destination = %destination,
                        "dropping guest UDP datagram because the host relay queue is full"
                    );
                    false
                }
                Err(TrySendError::Disconnected(payload)) => {
                    self.flows.remove(&flow_key);
                    let Some(flow) = create_udp_flow(guest_source, destination, relay_wake.clone())
                    else {
                        tracing::warn!(
                            guest_source = %guest_source,
                            destination = %destination,
                            "dropping guest UDP datagram because the relay flow could not be recreated"
                        );
                        return false;
                    };
                    let resend = flow.to_host.try_send(payload).is_ok();
                    self.flows.insert(flow_key, flow);
                    if !resend {
                        tracing::warn!(
                            guest_source = %guest_source,
                            destination = %destination,
                            "dropping guest UDP datagram because the recreated relay queue rejected it"
                        );
                    }
                    resend
                }
            }
        } else {
            false
        }
    }
}

fn create_udp_flow(
    guest_source: SocketAddr,
    destination: SocketAddr,
    relay_wake: Arc<WakePipe>,
) -> Option<TrackedUdpFlow> {
    let (to_host_tx, to_host_rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
    let (from_host_tx, from_host_rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
    spawn_udp_relay(destination, to_host_rx, from_host_tx, relay_wake)?;
    tracing::debug!(
        guest_source = %guest_source,
        destination = %destination,
        "created virtio UDP flow"
    );
    Some(TrackedUdpFlow {
        guest_source,
        destination,
        to_host: to_host_tx,
        from_host: from_host_rx,
        pending_host_payloads: VecDeque::new(),
        last_activity: Instant::now(),
    })
}

fn spawn_udp_relay(
    destination: SocketAddr,
    from_smoltcp: Receiver<Vec<u8>>,
    to_smoltcp: SyncSender<Vec<u8>>,
    relay_wake: Arc<WakePipe>,
) -> Option<()> {
    let thread_name = format!("smolvm-udp-{}", destination.port());
    if let Err(err) = thread::Builder::new()
        .name(thread_name)
        .spawn(move || run_udp_relay(destination, from_smoltcp, to_smoltcp, relay_wake))
    {
        tracing::warn!(
            destination = %destination,
            error = %err,
            "failed to spawn virtio UDP relay thread"
        );
        return None;
    }
    Some(())
}

fn run_udp_relay(
    destination: SocketAddr,
    from_smoltcp: Receiver<Vec<u8>>,
    to_smoltcp: SyncSender<Vec<u8>>,
    relay_wake: Arc<WakePipe>,
) {
    match udp_relay_loop(destination, from_smoltcp, to_smoltcp, relay_wake) {
        Ok(()) => tracing::debug!(destination = %destination, "virtio UDP relay thread stopped"),
        Err(err) => tracing::warn!(
            destination = %destination,
            error = %err,
            "virtio UDP relay thread failed"
        ),
    }
}

fn udp_relay_loop(
    destination: SocketAddr,
    from_smoltcp: Receiver<Vec<u8>>,
    to_smoltcp: SyncSender<Vec<u8>>,
    relay_wake: Arc<WakePipe>,
) -> io::Result<()> {
    let bind_addr = match destination {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => {
            return Err(io::Error::other(
                "virtio UDP relay currently supports only IPv4 destinations",
            ))
        }
    };

    let socket = HostUdpSocket::bind(bind_addr)?;
    socket.connect(destination)?;
    socket.set_nonblocking(true)?;

    let mut read_buffer = [0u8; RELAY_BUFFER_BYTES];

    loop {
        let mut did_work = false;

        loop {
            match from_smoltcp.try_recv() {
                Ok(payload) => {
                    let _ = socket.send(&payload)?;
                    did_work = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }

        loop {
            match socket.recv(&mut read_buffer) {
                Ok(bytes_read) => {
                    let payload = read_buffer[..bytes_read].to_vec();
                    match to_smoltcp.try_send(payload) {
                        Ok(()) => {
                            relay_wake.wake();
                            did_work = true;
                        }
                        Err(TrySendError::Full(_)) => {
                            tracing::warn!(
                                destination = %destination,
                                "dropping host UDP datagram because the guest relay queue is full"
                            );
                            did_work = true;
                        }
                        Err(TrySendError::Disconnected(_)) => return Ok(()),
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }

        if !did_work {
            thread::sleep(RELAY_IDLE_SLEEP);
        }
    }
}

fn send_host_payload(
    sockets: &mut SocketSet<'_>,
    handle: SocketHandle,
    destination: SocketAddr,
    guest_source: SocketAddr,
    payload: &[u8],
) -> Result<(), SendError> {
    let SocketAddr::V4(destination_v4) = destination else {
        return Err(SendError::Unaddressable);
    };
    let SocketAddr::V4(guest_source_v4) = guest_source else {
        return Err(SendError::Unaddressable);
    };

    let socket = sockets.get_mut::<UdpSocket>(handle);
    socket.send_slice(
        payload,
        UdpMetadata {
            endpoint: IpEndpoint::new(
                IpAddress::Ipv4(*guest_source_v4.ip()),
                guest_source_v4.port(),
            ),
            local_address: Some(IpAddress::Ipv4(*destination_v4.ip())),
            meta: Default::default(),
        },
    )
}

fn socket_addr_from_ip_endpoint(endpoint: IpEndpoint) -> Option<SocketAddr> {
    let IpAddress::Ipv4(ip) = endpoint.addr;

    Some(SocketAddr::new(IpAddr::V4(ip), endpoint.port))
}
