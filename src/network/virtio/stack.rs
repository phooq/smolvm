//! Host-side smoltcp runtime for the virtio-net backend.

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
                    eprintln!("virtio-net: dropping unsupported guest UDP datagram");
                    device.drop_staged_frame();
                }
            }
        }

        flush_interface_egress(&mut interface, &mut device, &mut sockets, now);
        interface.poll_maintenance(now);
        wake_guest_if_needed(&queues, &device);

        relays.relay_data(&mut sockets);
        process_dns_queries(dns_socket_handle, &mut sockets);

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
    interface
        .routes_mut()
        .add_default_ipv4_route(config.gateway_ipv4)
        .expect("failed to add default IPv4 route");
    interface.set_any_ip(true);
    interface
}

fn add_dns_socket(sockets: &mut SocketSet<'_>, gateway_ipv4: Ipv4Addr) -> SocketHandle {
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
    loop {
        let result = interface.poll_egress(now, device, sockets);
        if matches!(result, PollResult::None) {
            break;
        }
    }
}

fn wake_guest_if_needed(queues: &NetworkFrameQueues, device: &VirtioNetworkDevice) {
    if device.frames_emitted.swap(false, Ordering::Relaxed) {
        queues.host_wake.wake();
    }
}

fn smoltcp_now(start: StdInstant) -> Instant {
    Instant::from_micros(start.elapsed().as_micros() as i64)
}

fn classify_guest_frame(frame: &[u8]) -> FrameAction {
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
