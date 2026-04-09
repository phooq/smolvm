//! Guest-side virtio-net configuration from `SMOLVM_NETWORK_*`.

use std::net::Ipv4Addr;

/// Configure the guest network interface from host-provided environment.
///
/// Returns `Ok(false)` when virtio-net is not enabled for this boot.
pub fn configure_from_env() -> Result<bool, String> {
    let backend = match std::env::var("SMOLVM_NETWORK_BACKEND") {
        Ok(value) if !value.is_empty() => value,
        _ => return Ok(false),
    };

    if backend != "virtio" {
        return Err(format!(
            "unsupported SMOLVM_NETWORK_BACKEND value: {}",
            backend
        ));
    }

    let guest_ip = env_ipv4("SMOLVM_NETWORK_GUEST_IP")?;
    let gateway = env_ipv4("SMOLVM_NETWORK_GATEWAY")?;
    let prefix_len = env_u8("SMOLVM_NETWORK_PREFIX_LEN")?;
    let guest_mac = env_mac("SMOLVM_NETWORK_GUEST_MAC")?;
    let dns_server = env_ipv4("SMOLVM_NETWORK_DNS")?;

    linux::configure_interface(
        "eth0", guest_mac, 1500, guest_ip, prefix_len, gateway, dns_server,
    )?;
    Ok(true)
}

fn env_ipv4(name: &str) -> Result<Ipv4Addr, String> {
    let value = std::env::var(name).map_err(|_| format!("missing {}", name))?;
    value
        .parse::<Ipv4Addr>()
        .map_err(|_| format!("invalid IPv4 address for {}: {}", name, value))
}

fn env_u8(name: &str) -> Result<u8, String> {
    let value = std::env::var(name).map_err(|_| format!("missing {}", name))?;
    value
        .parse::<u8>()
        .map_err(|_| format!("invalid integer for {}: {}", name, value))
}

fn env_mac(name: &str) -> Result<[u8; 6], String> {
    let value = std::env::var(name).map_err(|_| format!("missing {}", name))?;
    parse_mac(&value)
}

fn parse_mac(value: &str) -> Result<[u8; 6], String> {
    let mut mac = [0u8; 6];
    let mut count = 0usize;
    for (index, part) in value.split(':').enumerate() {
        if index >= 6 {
            return Err(format!("invalid MAC address: {}", value));
        }
        mac[index] =
            u8::from_str_radix(part, 16).map_err(|_| format!("invalid MAC octet: {}", part))?;
        count = index + 1;
    }
    if count != 6 {
        return Err(format!("invalid MAC address: {}", value));
    }
    Ok(mac)
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
mod linux {
    use std::net::Ipv4Addr;

    #[allow(clippy::too_many_arguments)]
    pub fn configure_interface(
        _ifname: &str,
        _mac: [u8; 6],
        _mtu: u16,
        _address: Ipv4Addr,
        _prefix_len: u8,
        _gateway: Ipv4Addr,
        _dns_server: Ipv4Addr,
    ) -> Result<(), String> {
        Err("guest virtio networking is only supported on Linux".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mac_accepts_six_octets() {
        assert_eq!(
            parse_mac("02:53:4d:00:00:02").unwrap(),
            [0x02, 0x53, 0x4d, 0x00, 0x00, 0x02]
        );
    }

    #[test]
    fn parse_mac_rejects_invalid_input() {
        assert!(parse_mac("02:53:4d").is_err());
        assert!(parse_mac("zz:53:4d:00:00:02").is_err());
    }
}
