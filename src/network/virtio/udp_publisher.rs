//! Host->guest UDP publishing for the virtio-net backend.
//!
//! Context
//! =======
//!
//! General UDP relay already covers guest-originated flows:
//!
//! ```text
//! guest UDP -> host UdpSocket -> remote server -> guest reply
//! ```
//!
//! Published UDP ports invert the direction:
//!
//! ```text
//! host peer -> localhost:HOST/udp -> guest_ip:GUEST/udp
//!            <- gateway_ip:ephemeral <- guest reply
//! ```
//!
//! The host-side listener thread keeps the published host `UdpSocket` bound to
//! `localhost:HOST`. The poll loop allocates a per-peer gateway source port so
//! the guest can send replies back through smoltcp. Those replies are then
//! handed back to the listener thread, which writes them to the original host
//! peer using the published host port.

use crate::network::virtio::publisher::{PublishedUdpDatagram, PublishedUdpReply};
use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::udp::{PacketBuffer, PacketMetadata, Socket as UdpSocket, UdpMetadata};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant};

const UDP_PACKET_SLOTS: usize = 8;
const UDP_BUFFER_BYTES: usize = 8 * 1024;
const MAX_FLOWS: usize = 256;
const FLOW_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const EPHEMERAL_PORT_START: u16 = 40_000;
const EPHEMERAL_PORT_END: u16 = 60_000;

/// Summary counters for published UDP activity.
#[derive(Debug, Clone, Copy, Default)]
pub struct PublishedUdpStats {
    /// Host datagrams successfully written toward the guest.
    pub host_datagrams_forwarded: u64,
    /// Guest replies successfully written back to host peers.
    pub guest_datagrams_forwarded: u64,
    /// Host datagrams dropped before they could reach the guest.
    pub host_datagrams_dropped: u64,
    /// Guest replies dropped before they could be written back to host peers.
    pub guest_datagrams_dropped: u64,
}

struct PublishedUdpFlow {
    handle: SocketHandle,
    host_port: u16,
    guest_port: u16,
    peer_addr: SocketAddr,
    gateway_source_port: u16,
    reply_tx: SyncSender<PublishedUdpReply>,
    last_activity: Instant,
}

/// Poll-loop-owned table for published host->guest UDP flows.
pub struct PublishedUdpTable {
    flows: HashMap<(u16, SocketAddr), PublishedUdpFlow>,
    next_gateway_port: u16,
    max_flows: usize,
    stats: PublishedUdpStats,
}

impl PublishedUdpTable {
    /// Create a new published UDP table with a bounded flow count.
    pub fn new(max_flows: Option<usize>) -> Self {
        Self {
            flows: HashMap::new(),
            next_gateway_port: EPHEMERAL_PORT_START,
            max_flows: max_flows.unwrap_or(MAX_FLOWS),
            stats: PublishedUdpStats::default(),
        }
    }

    /// Drain host datagrams from published UDP listeners and write them into smoltcp.
    pub fn relay_host_datagrams(
        &mut self,
        incoming_datagrams: &mut Option<Receiver<PublishedUdpDatagram>>,
        reply_routes: &HashMap<u16, SyncSender<PublishedUdpReply>>,
        gateway_ipv4: Ipv4Addr,
        guest_ipv4: Ipv4Addr,
        sockets: &mut SocketSet<'_>,
    ) {
        let mut disconnected = false;

        if let Some(receiver) = incoming_datagrams.as_mut() {
            loop {
                match receiver.try_recv() {
                    Ok(datagram) => {
                        let Some(flow_key) =
                            self.ensure_flow(&datagram, reply_routes, gateway_ipv4, sockets)
                        else {
                            self.stats.host_datagrams_dropped += 1;
                            continue;
                        };

                        let send_result = {
                            let flow = self.flows.get_mut(&flow_key).expect("flow just created");
                            flow.last_activity = Instant::now();
                            let socket = sockets.get_mut::<UdpSocket>(flow.handle);
                            socket.send_slice(
                                &datagram.payload,
                                UdpMetadata {
                                    endpoint: IpEndpoint::new(
                                        IpAddress::Ipv4(guest_ipv4),
                                        datagram.guest_port,
                                    ),
                                    local_address: Some(IpAddress::Ipv4(gateway_ipv4)),
                                    meta: Default::default(),
                                },
                            )
                        };

                        if send_result.is_ok() {
                            self.stats.host_datagrams_forwarded += 1;
                        } else {
                            self.stats.host_datagrams_dropped += 1;
                            tracing::warn!(
                                host_port = datagram.host_port,
                                guest_port = datagram.guest_port,
                                peer_addr = %datagram.peer_addr,
                                payload_len = datagram.payload.len(),
                                "dropping published UDP datagram because the guest socket cannot send yet"
                            );
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        if disconnected {
            *incoming_datagrams = None;
        }
    }

    /// Drain guest replies from smoltcp and hand them back to the host listener threads.
    pub fn relay_guest_replies(&mut self, sockets: &mut SocketSet<'_>) {
        let flow_keys: Vec<(u16, SocketAddr)> = self.flows.keys().copied().collect();
        let mut flows_to_remove = Vec::new();

        for flow_key in flow_keys {
            let Some(flow) = self.flows.get_mut(&flow_key) else {
                continue;
            };
            let mut remove_flow = false;

            loop {
                let received = {
                    let socket = sockets.get_mut::<UdpSocket>(flow.handle);
                    match socket.recv() {
                        Ok((payload, _metadata)) => Some(payload.to_vec()),
                        Err(_) => None,
                    }
                };

                let Some(payload) = received else {
                    break;
                };

                flow.last_activity = Instant::now();
                match flow.reply_tx.try_send(PublishedUdpReply {
                    peer_addr: flow.peer_addr,
                    payload,
                }) {
                    Ok(()) => {
                        self.stats.guest_datagrams_forwarded += 1;
                    }
                    Err(TrySendError::Full(reply)) => {
                        self.stats.guest_datagrams_dropped += 1;
                        tracing::warn!(
                            host_port = flow.host_port,
                            guest_port = flow.guest_port,
                            peer_addr = %reply.peer_addr,
                            "dropping published UDP reply because the host egress queue is full"
                        );
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        remove_flow = true;
                        break;
                    }
                }
            }

            if remove_flow {
                flows_to_remove.push(flow_key);
            }
        }

        for flow_key in flows_to_remove {
            self.remove_flow(flow_key, sockets);
        }
    }

    /// Remove idle published UDP flows and their guest-facing smoltcp sockets.
    pub fn cleanup_idle(&mut self, sockets: &mut SocketSet<'_>) {
        let now = Instant::now();
        let stale_flows: Vec<(u16, SocketAddr)> = self
            .flows
            .iter()
            .filter_map(|(flow_key, flow)| {
                if now.duration_since(flow.last_activity) >= FLOW_IDLE_TIMEOUT {
                    Some(*flow_key)
                } else {
                    None
                }
            })
            .collect();

        for flow_key in stale_flows {
            self.remove_flow(flow_key, sockets);
        }
    }

    /// Number of active published UDP flows.
    pub fn active_flow_count(&self) -> usize {
        self.flows.len()
    }

    /// Snapshot of accumulated published UDP counters.
    pub fn stats(&self) -> PublishedUdpStats {
        self.stats
    }

    fn ensure_flow(
        &mut self,
        datagram: &PublishedUdpDatagram,
        reply_routes: &HashMap<u16, SyncSender<PublishedUdpReply>>,
        gateway_ipv4: Ipv4Addr,
        sockets: &mut SocketSet<'_>,
    ) -> Option<(u16, SocketAddr)> {
        let flow_key = (datagram.host_port, datagram.peer_addr);
        if self.flows.contains_key(&flow_key) {
            return Some(flow_key);
        }

        if self.flows.len() >= self.max_flows {
            tracing::warn!(
                host_port = datagram.host_port,
                guest_port = datagram.guest_port,
                peer_addr = %datagram.peer_addr,
                "dropping published UDP datagram because the flow table is full"
            );
            return None;
        }

        let reply_tx = reply_routes.get(&datagram.host_port)?.clone();
        let gateway_source_port = self.allocate_gateway_source_port()?;

        let rx_meta = vec![PacketMetadata::EMPTY; UDP_PACKET_SLOTS];
        let tx_meta = vec![PacketMetadata::EMPTY; UDP_PACKET_SLOTS];
        let rx_buffer = PacketBuffer::new(rx_meta, vec![0u8; UDP_BUFFER_BYTES]);
        let tx_buffer = PacketBuffer::new(tx_meta, vec![0u8; UDP_BUFFER_BYTES]);
        let mut socket = UdpSocket::new(rx_buffer, tx_buffer);
        if socket
            .bind(IpListenEndpoint {
                addr: Some(IpAddress::Ipv4(gateway_ipv4)),
                port: gateway_source_port,
            })
            .is_err()
        {
            tracing::warn!(
                host_port = datagram.host_port,
                guest_port = datagram.guest_port,
                peer_addr = %datagram.peer_addr,
                gateway_source_port,
                "dropping published UDP datagram because the guest-facing socket could not be created"
            );
            return None;
        }

        let handle = sockets.add(socket);
        self.flows.insert(
            flow_key,
            PublishedUdpFlow {
                handle,
                host_port: datagram.host_port,
                guest_port: datagram.guest_port,
                peer_addr: datagram.peer_addr,
                gateway_source_port,
                reply_tx,
                last_activity: Instant::now(),
            },
        );

        tracing::debug!(
            host_port = datagram.host_port,
            guest_port = datagram.guest_port,
            peer_addr = %datagram.peer_addr,
            gateway_source_port,
            "created published UDP flow"
        );

        Some(flow_key)
    }

    fn allocate_gateway_source_port(&mut self) -> Option<u16> {
        for _ in EPHEMERAL_PORT_START..=EPHEMERAL_PORT_END {
            let candidate = self.next_gateway_port;
            self.next_gateway_port = if self.next_gateway_port == EPHEMERAL_PORT_END {
                EPHEMERAL_PORT_START
            } else {
                self.next_gateway_port + 1
            };

            if candidate == 53 {
                continue;
            }

            if self
                .flows
                .values()
                .all(|flow| flow.gateway_source_port != candidate)
            {
                return Some(candidate);
            }
        }

        None
    }

    fn remove_flow(&mut self, flow_key: (u16, SocketAddr), sockets: &mut SocketSet<'_>) {
        if let Some(flow) = self.flows.remove(&flow_key) {
            sockets.remove(flow.handle);
            tracing::debug!(
                host_port = flow.host_port,
                guest_port = flow.guest_port,
                peer_addr = %flow.peer_addr,
                gateway_source_port = flow.gateway_source_port,
                "removed published UDP flow"
            );
        }
    }
}
