//! Linux network configuration helpers for `eth0`.

use std::net::Ipv4Addr;

pub fn configure_interface(
    ifname: &str,
    mac: [u8; 6],
    mtu: u16,
    address: Ipv4Addr,
    prefix_len: u8,
    gateway: Ipv4Addr,
    dns_server: Ipv4Addr,
) -> Result<(), String> {
    let ifindex = get_ifindex(ifname)?;
    set_mac_address(ifname, &mac)?;
    set_mtu(ifname, mtu)?;
    add_address_v4(ifindex, address, prefix_len)?;
    bring_interface_up(ifname)?;
    add_default_route_v4(gateway)?;
    write_resolv_conf(dns_server)?;
    Ok(())
}

fn get_ifindex(ifname: &str) -> Result<u32, String> {
    // SAFETY: `ifreq` is plain old data; zeroed initialization is valid.
    unsafe {
        let mut ifr: libc::ifreq = std::mem::zeroed();
        copy_ifname(&mut ifr, ifname)?;

        let sock = socket_fd()?;
        if libc::ioctl(sock, libc::SIOCGIFINDEX as _, &mut ifr) < 0 {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(format!("SIOCGIFINDEX failed for {}: {}", ifname, err));
        }
        libc::close(sock);

        Ok(ifr.ifr_ifru.ifru_ifindex as u32)
    }
}

fn set_mac_address(ifname: &str, mac: &[u8; 6]) -> Result<(), String> {
    // SAFETY: `ifreq` is plain old data; zeroed initialization is valid.
    unsafe {
        let mut ifr: libc::ifreq = std::mem::zeroed();
        copy_ifname(&mut ifr, ifname)?;

        ifr.ifr_ifru.ifru_hwaddr.sa_family = libc::ARPHRD_ETHER;
        ifr.ifr_ifru.ifru_hwaddr.sa_data[..6]
            .copy_from_slice(&mac.map(|byte| byte as libc::c_char));

        let sock = socket_fd()?;
        if libc::ioctl(sock, libc::SIOCSIFHWADDR as _, &ifr) < 0 {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(format!("SIOCSIFHWADDR failed for {}: {}", ifname, err));
        }
        libc::close(sock);
    }
    Ok(())
}

fn set_mtu(ifname: &str, mtu: u16) -> Result<(), String> {
    // SAFETY: `ifreq` is plain old data; zeroed initialization is valid.
    unsafe {
        let mut ifr: libc::ifreq = std::mem::zeroed();
        copy_ifname(&mut ifr, ifname)?;
        ifr.ifr_ifru.ifru_mtu = mtu as libc::c_int;

        let sock = socket_fd()?;
        if libc::ioctl(sock, libc::SIOCSIFMTU as _, &ifr) < 0 {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(format!("SIOCSIFMTU failed for {}: {}", ifname, err));
        }
        libc::close(sock);
    }
    Ok(())
}

fn bring_interface_up(ifname: &str) -> Result<(), String> {
    // SAFETY: `ifreq` is plain old data; zeroed initialization is valid.
    unsafe {
        let mut ifr: libc::ifreq = std::mem::zeroed();
        copy_ifname(&mut ifr, ifname)?;

        let sock = socket_fd()?;
        if libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut ifr) < 0 {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(format!("SIOCGIFFLAGS failed for {}: {}", ifname, err));
        }

        ifr.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
        if libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &ifr) < 0 {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(format!("SIOCSIFFLAGS failed for {}: {}", ifname, err));
        }
        libc::close(sock);
    }
    Ok(())
}

fn add_address_v4(ifindex: u32, address: Ipv4Addr, prefix_len: u8) -> Result<(), String> {
    let address_bytes = address.octets();
    netlink_newaddr(ifindex, prefix_len, &address_bytes).map_err(|err| {
        format!(
            "failed to add IPv4 address {}/{}: {}",
            address, prefix_len, err
        )
    })
}

fn add_default_route_v4(gateway: Ipv4Addr) -> Result<(), String> {
    let gateway_bytes = gateway.octets();
    netlink_newroute(&gateway_bytes)
        .map_err(|err| format!("failed to add default route via {}: {}", gateway, err))
}

fn write_resolv_conf(dns_server: Ipv4Addr) -> Result<(), String> {
    std::fs::write("/etc/resolv.conf", format!("nameserver {}\n", dns_server))
        .map_err(|err| format!("failed to write /etc/resolv.conf: {}", err))
}

fn socket_fd() -> Result<libc::c_int, String> {
    // SAFETY: `socket` is a standard libc call with valid arguments.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!(
            "failed to create socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(fd)
}

fn copy_ifname(ifr: &mut libc::ifreq, ifname: &str) -> Result<(), String> {
    let bytes = ifname.as_bytes();
    if bytes.len() >= libc::IFNAMSIZ {
        return Err(format!("interface name too long: {}", ifname));
    }

    // SAFETY: `ifr_name` is large enough because of the length check above.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr().cast(),
            bytes.len(),
        );
    }

    Ok(())
}

#[repr(C)]
struct IfAddrMsg {
    ifa_family: u8,
    ifa_prefixlen: u8,
    ifa_flags: u8,
    ifa_scope: u8,
    ifa_index: u32,
}

#[repr(C)]
struct RtMsg {
    rtm_family: u8,
    rtm_dst_len: u8,
    rtm_src_len: u8,
    rtm_tos: u8,
    rtm_table: u8,
    rtm_protocol: u8,
    rtm_scope: u8,
    rtm_type: u8,
    rtm_flags: u32,
}

const NLMSG_HDRLEN: usize = 16;
const IFADDRMSG_LEN: usize = 8;
const RTMSG_LEN: usize = 12;
const RTA_HDRLEN: usize = 4;

const _: () = assert!(std::mem::size_of::<libc::nlmsghdr>() == NLMSG_HDRLEN);
const _: () = assert!(std::mem::size_of::<IfAddrMsg>() == IFADDRMSG_LEN);
const _: () = assert!(std::mem::size_of::<RtMsg>() == RTMSG_LEN);

fn netlink_newaddr(ifindex: u32, prefix_len: u8, address: &[u8]) -> std::io::Result<()> {
    let rta_len = rta_space(address.len());
    let msg_len = NLMSG_HDRLEN + IFADDRMSG_LEN + (rta_len * 2);
    let mut buf = vec![0u8; nlmsg_align(msg_len)];

    let nlh = buf.as_mut_ptr().cast::<libc::nlmsghdr>();
    // SAFETY: `buf` is large enough for `nlmsghdr`.
    unsafe {
        (*nlh).nlmsg_len = msg_len as u32;
        (*nlh).nlmsg_type = libc::RTM_NEWADDR;
        (*nlh).nlmsg_flags =
            (libc::NLM_F_REQUEST | libc::NLM_F_ACK | libc::NLM_F_CREATE | libc::NLM_F_EXCL) as u16;
        (*nlh).nlmsg_seq = 1;
    }

    let ifa = unsafe { buf.as_mut_ptr().add(NLMSG_HDRLEN).cast::<IfAddrMsg>() };
    // SAFETY: `buf` is large enough for `IfAddrMsg`.
    unsafe {
        (*ifa).ifa_family = libc::AF_INET as u8;
        (*ifa).ifa_prefixlen = prefix_len;
        (*ifa).ifa_flags = 0;
        (*ifa).ifa_scope = libc::RT_SCOPE_UNIVERSE;
        (*ifa).ifa_index = ifindex;
    }

    let mut offset = NLMSG_HDRLEN + IFADDRMSG_LEN;
    write_rta(&mut buf[offset..], libc::IFA_ADDRESS, address);
    offset += rta_space(address.len());
    write_rta(&mut buf[offset..], libc::IFA_LOCAL, address);

    netlink_send(&buf)
}

fn netlink_newroute(gateway: &[u8]) -> std::io::Result<()> {
    let rta_len = rta_space(gateway.len());
    let msg_len = NLMSG_HDRLEN + RTMSG_LEN + rta_len;
    let mut buf = vec![0u8; nlmsg_align(msg_len)];

    let nlh = buf.as_mut_ptr().cast::<libc::nlmsghdr>();
    // SAFETY: `buf` is large enough for `nlmsghdr`.
    unsafe {
        (*nlh).nlmsg_len = msg_len as u32;
        (*nlh).nlmsg_type = libc::RTM_NEWROUTE;
        (*nlh).nlmsg_flags =
            (libc::NLM_F_REQUEST | libc::NLM_F_ACK | libc::NLM_F_CREATE | libc::NLM_F_EXCL) as u16;
        (*nlh).nlmsg_seq = 2;
    }

    let rtm = unsafe { buf.as_mut_ptr().add(NLMSG_HDRLEN).cast::<RtMsg>() };
    // SAFETY: `buf` is large enough for `RtMsg`.
    unsafe {
        (*rtm).rtm_family = libc::AF_INET as u8;
        (*rtm).rtm_dst_len = 0;
        (*rtm).rtm_src_len = 0;
        (*rtm).rtm_tos = 0;
        (*rtm).rtm_table = libc::RT_TABLE_MAIN;
        (*rtm).rtm_protocol = libc::RTPROT_BOOT;
        (*rtm).rtm_scope = libc::RT_SCOPE_UNIVERSE;
        (*rtm).rtm_type = libc::RTN_UNICAST;
        (*rtm).rtm_flags = 0;
    }

    let offset = NLMSG_HDRLEN + RTMSG_LEN;
    write_rta(&mut buf[offset..], libc::RTA_GATEWAY, gateway);
    netlink_send(&buf)
}

fn netlink_send(msg: &[u8]) -> std::io::Result<()> {
    // SAFETY: all libc calls use valid buffers and checked lengths.
    unsafe {
        let sock = libc::socket(libc::AF_NETLINK, libc::SOCK_DGRAM, libc::NETLINK_ROUTE);
        if sock < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut sockaddr: libc::sockaddr_nl = std::mem::zeroed();
        sockaddr.nl_family = libc::AF_NETLINK as u16;
        if libc::bind(
            sock,
            (&sockaddr as *const libc::sockaddr_nl).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as u32,
        ) < 0
        {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(err);
        }

        if libc::send(sock, msg.as_ptr().cast(), msg.len(), 0) < 0 {
            let err = std::io::Error::last_os_error();
            libc::close(sock);
            return Err(err);
        }

        let mut ack = [0u8; 1024];
        let bytes = libc::recv(sock, ack.as_mut_ptr().cast(), ack.len(), 0);
        libc::close(sock);
        if bytes < 0 {
            return Err(std::io::Error::last_os_error());
        }

        if (bytes as usize) >= NLMSG_HDRLEN + 4 {
            let nlh = ack.as_ptr().cast::<libc::nlmsghdr>();
            if (*nlh).nlmsg_type == libc::NLMSG_ERROR as u16 {
                let err =
                    i32::from_ne_bytes(ack[NLMSG_HDRLEN..NLMSG_HDRLEN + 4].try_into().unwrap());
                if err < 0 {
                    return Err(std::io::Error::from_raw_os_error(-err));
                }
            }
        }

        Ok(())
    }
}

fn nlmsg_align(len: usize) -> usize {
    (len + 3) & !3
}

fn rta_space(data_len: usize) -> usize {
    nlmsg_align(RTA_HDRLEN + data_len)
}

fn write_rta(buf: &mut [u8], rta_type: u16, data: &[u8]) {
    let rta_len = (RTA_HDRLEN + data.len()) as u16;
    buf[0..2].copy_from_slice(&rta_len.to_ne_bytes());
    buf[2..4].copy_from_slice(&rta_type.to_ne_bytes());
    buf[RTA_HDRLEN..RTA_HDRLEN + data.len()].copy_from_slice(data);
}
