//! Addressing helpers for the virtio-net backend.

use std::net::Ipv4Addr;

/// Static guest network configuration for the virtio-net MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestNetworkConfig {
    /// Guest IPv4 address.
    pub guest_ip: Ipv4Addr,
    /// Gateway IPv4 address.
    pub gateway_ip: Ipv4Addr,
    /// Prefix length.
    pub prefix_len: u8,
    /// Guest MAC address.
    pub guest_mac: [u8; 6],
    /// Gateway MAC address.
    pub gateway_mac: [u8; 6],
    /// DNS server address presented to the guest.
    pub dns_server: Ipv4Addr,
}

impl GuestNetworkConfig {
    /// Default Phase 1 guest network configuration.
    pub const fn default_mvp() -> Self {
        Self {
            guest_ip: Ipv4Addr::new(100, 96, 0, 2),
            gateway_ip: Ipv4Addr::new(100, 96, 0, 1),
            prefix_len: 30,
            guest_mac: [0x02, 0x53, 0x4d, 0x00, 0x00, 0x02],
            gateway_mac: [0x02, 0x53, 0x4d, 0x00, 0x00, 0x01],
            dns_server: Ipv4Addr::new(100, 96, 0, 1),
        }
    }
}
