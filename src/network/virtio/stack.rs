//! Host-side smoltcp runtime for the virtio-net backend.
//!
//! Context
//! =======
//!
//! This file is the in-process "gateway" that sits behind the guest's virtio
//! NIC. It does not configure the guest interface; that already happened inside
//! `smolvm-agent`. Instead, this module:
//! - receives raw Ethernet frames coming out of libkrun
//! - feeds them into smoltcp as if smolvm were the guest's next-hop gateway
//! - forwards guest DNS queries to a host UDP socket
//! - relays guest TCP streams to host `TcpStream`s
//!
//! Conceptually, it plays the role of a tiny virtual router/NAT-side gateway:
//!
//! ```text
//! guest eth0
//!   -> Ethernet frame
//!   -> Frame queues
//!   -> smoltcp Interface (gateway MAC/IP)
//!   -> protocol-specific handling:
//!        - TCP  -> host relay threads
//!        - DNS  -> host UDP socket
//!        - other supported egress -> future phases
//!   -> outbound network
//! ```
//!
//! Poll-loop-centric view:
//!
//! ```text
//! guest_to_host queue
//!   -> VirtioNetworkDevice::stage_next_frame()
//!   -> classify_guest_frame()
//!   -> smoltcp ingress
//!   -> protocol-specific side effects
//!        - TCP SYN  -> create relay/socket state
//!        - DNS UDP  -> gateway UDP socket
//!        - other UDP-> drop for now
//!   -> smoltcp egress
//!   -> host_to_guest queue
//!   -> FrameStream writer
//! ```
//!
//! Runtime control flow:
//!
//! ```text
//! new guest frame         -> guest_wake  -> poll loop
//! host relay has data     -> relay_wake  -> poll loop
//! smoltcp emitted frames  -> host_wake   -> frame writer
//! ```
//!
//! Important limitation of the current MVP:
//! - TCP is supported via host relay threads
//! - DNS is supported via a gateway UDP socket plus host UDP forwarding
//! - other UDP is intentionally dropped
//! - there is no policy engine or port publishing yet
//!
//! There is no single shell command equivalent for this module. The closest
//! operational mental model is "userspace network stack + gateway daemon".

use crate::data::network::DEFAULT_DNS_ADDR;
use crate::network::virtio::device::VirtioNetworkDevice;
use crate::network::virtio::queues::NetworkFrameQueues;
use crate::network::virtio::tcp_relay::{spawn_tcp_relay, TcpRelayTable};
use smoltcp::iface::{
    Config, Interface, PollIngressSingleResult, PollResult, SocketHandle, SocketSet,
};
use smoltcp::socket::udp::{PacketBuffer, PacketMetadata, Socket as UdpSocket, UdpMetadata};
use smoltcp::time::Instant;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, HardwareAddress, IpAddress, IpCidr,
    Ipv4Packet, TcpPacket, UdpPacket,
};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket as HostUdpSocket};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant as StdInstant};

const DNS_SOCKET_PORT: u16 = 53;
const DNS_PACKET_SLOTS: usize = 8;
const DNS_BUFFER_BYTES: usize = 2048;
const DEFAULT_IDLE_TIMEOUT_MS: i32 = 100;

/// Resolved network parameters for one guest NIC.
///
/// These are the host-side parameters for the virtual link. Note that the
/// smoltcp interface is configured with the *gateway* MAC/IP, because the host
/// runtime is acting as the guest-visible gateway endpoint.
#[derive(Debug, Clone, Copy)]
pub struct VirtioPollConfig {
    /// Host-side gateway MAC visible to the guest.
    pub gateway_mac: [u8; 6],
    /// Guest MAC address.
    pub guest_mac: [u8; 6],
    /// Gateway IPv4 address.
    pub gateway_ipv4: Ipv4Addr,
    /// Guest IPv4 address.
    pub guest_ipv4: Ipv4Addr,
    /// IP-level MTU.
    pub mtu: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameAction {
    TcpSyn {
        source: SocketAddr,
        destination: SocketAddr,
    },
    DnsQuery,
    UnsupportedUdp,
    Passthrough,
}

/// Start the dedicated smoltcp poll thread for the virtio-net backend.
///
/// This creates one long-lived poll loop thread per guest NIC. That thread owns
/// the smoltcp `Interface`, its socket set, and the TCP relay table.
///
/// Ownership boundary:
/// - this thread owns all smoltcp state
/// - relay threads never touch smoltcp sockets directly
/// - frame bridge threads never parse protocols beyond raw Ethernet framing
pub fn start_network_stack(
    queues: Arc<NetworkFrameQueues>,
    config: VirtioPollConfig,
) -> std::io::Result<JoinHandle<()>> {
    eprintln!(
        "virtio-net: spawning poll thread guest_ip={} gateway_ip={} mtu={}",
        config.guest_ipv4, config.gateway_ipv4, config.mtu
    );
    thread::Builder::new()
        .name("smolvm-net-poll".into())
        .spawn(move || run_network_stack(queues, config))
}

fn run_network_stack(queues: Arc<NetworkFrameQueues>, config: VirtioPollConfig) {
    // Poll loop overview:
    //
    // 1. Drain staged guest Ethernet frames from the guest_to_host queue.
    // 2. Pre-classify them so we can create relay/socket state before smoltcp
    //    consumes the frame.
    // 3. Poll smoltcp ingress/egress.
    // 4. Forward DNS and relay TCP payloads.
    // 5. Sleep in poll(2) on wake pipes until guest frames, relay activity, or
    //    timers require more work.
    //
    // A useful mental model is:
    //
    //   queue input -> classify -> smoltcp -> protocol handling -> queue output
    eprintln!(
        "virtio-net: poll loop started guest_ip={} gateway_ip={}",
        config.guest_ipv4, config.gateway_ipv4
    );
    let clock = StdInstant::now();
    let mut device = VirtioNetworkDevice::new(queues.clone(), config.mtu);
    let mut interface = create_interface(&mut device, &config);
    let mut sockets = SocketSet::new(vec![]);
    let dns_socket_handle = add_dns_socket(&mut sockets, config.gateway_ipv4);
    let relay_wake = Arc::new(queues.relay_wake.clone());
    let mut relays = TcpRelayTable::new(None);

    // The smoltcp loop is driven by fd-based wakeups rather than busy spinning.
    // guest_wake  -> new guest frame or shutdown
    // relay_wake  -> host TCP relay thread produced data or shutdown
    let mut poll_fds = [
        libc::pollfd {
            fd: queues.guest_wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: queues.relay_wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    loop {
        if queues.is_shutting_down() {
            return;
        }
        let now = smoltcp_now(clock);

        while let Some(frame) = device.stage_next_frame() {
            // We inspect the frame before giving it to smoltcp because certain
            // flows need side effects first:
            // - TCP SYN: pre-create a matching smoltcp socket + relay entry
            // - DNS UDP: allow through for gateway-side forwarding
            // - other UDP: currently unsupported in the MVP
            match classify_guest_frame(frame) {
                FrameAction::TcpSyn {
                    source,
                    destination,
                } => {
                    eprintln!(
                        "virtio-net: guest TCP SYN source={} destination={}",
                        source, destination
                    );
                    if !relays.has_socket_for(&source, &destination) {
                        relays.create_tcp_socket(source, destination, &mut sockets);
                    }
                    if matches!(
                        interface.poll_ingress_single(now, &mut device, &mut sockets),
                        PollIngressSingleResult::None
                    ) {
                        device.drop_staged_frame();
                    }
                }
                FrameAction::DnsQuery | FrameAction::Passthrough => {
                    if matches!(
                        interface.poll_ingress_single(now, &mut device, &mut sockets),
                        PollIngressSingleResult::None
                    ) {
                        device.drop_staged_frame();
                    }
                }
                FrameAction::UnsupportedUdp => {
                    // Phase 1 only supports DNS over UDP. Other UDP traffic is
                    // intentionally dropped until a general UDP relay exists.
                    eprintln!("virtio-net: dropping unsupported guest UDP datagram");
                    device.drop_staged_frame();
                }
            }
        }

        // First egress pass: let smoltcp emit any packets caused by the most
        // recent ingress work before we service higher-level relays.
        flush_interface_egress(&mut interface, &mut device, &mut sockets, now);
        interface.poll_maintenance(now);
        wake_guest_if_needed(&queues, &device);

        // Move payloads between established smoltcp TCP sockets and host relay
        // threads, and service the DNS gateway socket.
        relays.relay_data(&mut sockets);
        process_dns_queries(dns_socket_handle, &mut sockets);

        // Once the guest-side TCP handshake is established inside smoltcp, we
        // can spawn the corresponding host relay thread.
        for connection in relays.take_new_connections(&mut sockets) {
            spawn_tcp_relay(
                connection.destination,
                connection.from_smoltcp,
                connection.to_smoltcp,
                relay_wake.clone(),
                connection.exit_state,
            );
        }

        relays.cleanup_closed(&mut sockets);

        // Second egress pass: DNS responses or relay data may have queued more
        // packets for the guest.
        flush_interface_egress(&mut interface, &mut device, &mut sockets, now);
        wake_guest_if_needed(&queues, &device);

        let timeout_ms = interface
            .poll_delay(now, &sockets)
            .map(|duration| duration.total_millis().min(i32::MAX as u64) as i32)
            .unwrap_or(DEFAULT_IDLE_TIMEOUT_MS);

        // SAFETY: both pollfds contain valid wake-pipe descriptors.
        unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as libc::nfds_t,
                timeout_ms,
            );
        }

        if poll_fds[0].revents & libc::POLLIN != 0 {
            queues.guest_wake.drain();
        }
        if poll_fds[1].revents & libc::POLLIN != 0 {
            queues.relay_wake.drain();
        }
    }
}

fn create_interface(device: &mut VirtioNetworkDevice, config: &VirtioPollConfig) -> Interface {
    // This interface models the host-side gateway endpoint, not the guest NIC.
    //
    // Equivalent conceptual state:
    //   MAC: config.gateway_mac
    //   IP : config.gateway_ipv4/30
    //
    // The guest IP exists as a peer on the same virtual link; it is not an
    // address owned by this interface.
    let mut interface = Interface::new(
        Config::new(HardwareAddress::Ethernet(EthernetAddress(
            config.gateway_mac,
        ))),
        device,
        Instant::ZERO,
    );
    interface.update_ip_addrs(|addresses| {
        addresses
            .push(IpCidr::new(IpAddress::Ipv4(config.gateway_ipv4), 30))
            .expect("failed to add gateway IPv4 address");
    });
    // The interface acts as the gateway and may need to answer packets for
    // destinations other than its directly assigned IP, so the route table and
    // "any IP" mode are opened up accordingly.
    interface
        .routes_mut()
        .add_default_ipv4_route(config.gateway_ipv4)
        .expect("failed to add default IPv4 route");
    interface.set_any_ip(true);
    interface
}

fn add_dns_socket(sockets: &mut SocketSet<'_>, gateway_ipv4: Ipv4Addr) -> SocketHandle {
    // Bind a UDP socket on gateway:53 inside smoltcp. Guest DNS packets hit
    // this socket first; we then proxy them to the host resolver path.
    let rx_meta = vec![PacketMetadata::EMPTY; DNS_PACKET_SLOTS];
    let tx_meta = vec![PacketMetadata::EMPTY; DNS_PACKET_SLOTS];
    let rx_buffer = PacketBuffer::new(rx_meta, vec![0u8; DNS_BUFFER_BYTES]);
    let tx_buffer = PacketBuffer::new(tx_meta, vec![0u8; DNS_BUFFER_BYTES]);
    let mut socket = UdpSocket::new(rx_buffer, tx_buffer);
    socket
        .bind(smoltcp::wire::IpListenEndpoint {
            addr: Some(gateway_ipv4.into()),
            port: DNS_SOCKET_PORT,
        })
        .expect("failed to bind gateway DNS socket");
    sockets.add(socket)
}

fn process_dns_queries(dns_socket_handle: SocketHandle, sockets: &mut SocketSet<'_>) {
    // Phase 1 DNS model:
    // guest UDP/53 -> smoltcp gateway socket -> host UDP socket -> upstream DNS
    //               <-               response bytes               <-
    let upstream_dns = match DEFAULT_DNS_ADDR {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(_) => return,
    };

    let socket = sockets.get_mut::<UdpSocket>(dns_socket_handle);
    while socket.can_recv() {
        let (query, metadata) = match socket.recv() {
            Ok(result) => result,
            Err(_) => break,
        };
        eprintln!(
            "virtio-net: forwarding guest DNS query guest={} local_address={:?} query_len={} upstream_dns={}",
            metadata.endpoint,
            metadata.local_address,
            query.len(),
            upstream_dns
        );
        let response = match forward_dns_query(upstream_dns, query) {
            Ok(response) => response,
            Err(err) => {
                eprintln!("virtio-net: host DNS forwarding failed error={}", err);
                continue;
            }
        };
        eprintln!(
            "virtio-net: forwarded DNS response back to guest guest={} response_len={}",
            metadata.endpoint,
            response.len()
        );

        let response_meta = UdpMetadata {
            endpoint: metadata.endpoint,
            local_address: metadata.local_address,
            meta: Default::default(),
        };
        let _ = socket.send_slice(&response, response_meta);
    }
}

fn forward_dns_query(upstream_dns: Ipv4Addr, query: &[u8]) -> std::io::Result<Vec<u8>> {
    // This is intentionally a plain host UDP exchange rather than a smoltcp
    // socket-to-socket relay. Once the guest packet reaches the gateway, the
    // simplest MVP path is to proxy it with the host kernel's UDP stack.
    //
    // Rough shell equivalent:
    //   send raw DNS message to `<upstream_dns>:53`
    //   wait up to 2 seconds for one reply
    let socket = HostUdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let local_addr = socket.local_addr()?;
    eprintln!(
        "virtio-net: sending DNS query to upstream resolver local_addr={} upstream_dns={} query_len={}",
        local_addr,
        upstream_dns,
        query.len()
    );
    socket.send_to(query, (upstream_dns, DNS_SOCKET_PORT))?;

    let mut buffer = vec![0u8; DNS_BUFFER_BYTES];
    let (bytes_read, _) = socket.recv_from(&mut buffer)?;
    buffer.truncate(bytes_read);
    eprintln!(
        "virtio-net: received DNS response from upstream resolver upstream_dns={} response_len={}",
        upstream_dns,
        buffer.len()
    );
    Ok(buffer)
}

fn flush_interface_egress(
    interface: &mut Interface,
    device: &mut VirtioNetworkDevice,
    sockets: &mut SocketSet<'_>,
    now: Instant,
) {
    // smoltcp may have multiple pending egress packets after a single ingress
    // event or timeout. Keep polling until the interface reports there is no
    // more immediate work.
    loop {
        let result = interface.poll_egress(now, device, sockets);
        if matches!(result, PollResult::None) {
            break;
        }
    }
}

fn wake_guest_if_needed(queues: &NetworkFrameQueues, device: &VirtioNetworkDevice) {
    // The device records only that "some frame was emitted". We convert that
    // sticky bit into one wake for the writer thread and let the writer drain
    // the entire host_to_guest queue.
    if device.frames_emitted.swap(false, Ordering::Relaxed) {
        queues.host_wake.wake();
    }
}

fn smoltcp_now(start: StdInstant) -> Instant {
    Instant::from_micros(start.elapsed().as_micros() as i64)
}

fn classify_guest_frame(frame: &[u8]) -> FrameAction {
    // This pre-parser is intentionally shallow. It only looks far enough to
    // decide whether the poll loop needs special handling before smoltcp sees
    // the frame.
    //
    // Current policy:
    // - TCP SYN  -> create relay/socket state before ingress
    // - UDP/53   -> allow as DNS
    // - other UDP-> drop in Phase 1
    // - anything else -> normal smoltcp ingress path
    let Ok(ethernet) = EthernetFrame::new_checked(frame) else {
        return FrameAction::Passthrough;
    };

    if ethernet.ethertype() != EthernetProtocol::Ipv4 {
        return FrameAction::Passthrough;
    }

    let Ok(ipv4) = Ipv4Packet::new_checked(ethernet.payload()) else {
        return FrameAction::Passthrough;
    };

    match ipv4.next_header() {
        smoltcp::wire::IpProtocol::Tcp => {
            classify_tcp(ipv4.payload(), ipv4.src_addr(), ipv4.dst_addr())
        }
        smoltcp::wire::IpProtocol::Udp => classify_udp(ipv4.payload()),
        _ => FrameAction::Passthrough,
    }
}

fn classify_tcp(payload: &[u8], source: Ipv4Addr, destination: Ipv4Addr) -> FrameAction {
    // New outgoing guest TCP connections are identified by the initial
    // SYN-without-ACK packet. That is the moment where we need a matching
    // smoltcp socket and later a host relay thread.
    let Ok(tcp) = TcpPacket::new_checked(payload) else {
        return FrameAction::Passthrough;
    };

    if tcp.syn() && !tcp.ack() && !tcp.rst() {
        FrameAction::TcpSyn {
            source: SocketAddr::new(source.into(), tcp.src_port()),
            destination: SocketAddr::new(destination.into(), tcp.dst_port()),
        }
    } else {
        FrameAction::Passthrough
    }
}

fn classify_udp(payload: &[u8]) -> FrameAction {
    // Phase 1 treats DNS as the only supported guest UDP protocol.
    let Ok(udp) = UdpPacket::new_checked(payload) else {
        return FrameAction::Passthrough;
    };

    if udp.dst_port() == DNS_SOCKET_PORT {
        eprintln!(
            "virtio-net: guest DNS UDP datagram received src_port={} dst_port={} payload_len={}",
            udp.src_port(),
            udp.dst_port(),
            udp.payload().len()
        );
        FrameAction::DnsQuery
    } else {
        FrameAction::UnsupportedUdp
    }
}
