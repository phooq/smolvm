//! Virtio-net backend implementation.

pub mod device;
pub mod frame_stream;
pub mod queues;
pub mod stack;
pub mod tcp_relay;

use crate::network::addressing::GuestNetworkConfig;
use crate::network::virtio::frame_stream::{start_frame_stream_bridge, FrameStreamBridge};
use crate::network::virtio::queues::{NetworkFrameQueues, DEFAULT_FRAME_QUEUE_CAPACITY};
use crate::network::virtio::stack::{start_network_stack, VirtioPollConfig};
use std::io;
use std::os::fd::RawFd;
use std::thread::JoinHandle;

/// Running host-side virtio-net runtime for one guest NIC.
pub struct VirtioNetworkRuntime {
    queues: std::sync::Arc<NetworkFrameQueues>,
    _frame_bridge: FrameStreamBridge,
    poll_handle: Option<JoinHandle<()>>,
}

/// Start the host-side virtio-net runtime for one guest NIC.
pub fn start_virtio_network(
    host_fd: RawFd,
    guest_network: GuestNetworkConfig,
) -> io::Result<VirtioNetworkRuntime> {
    eprintln!(
        "virtio-net: starting runtime host_fd={} guest_ip={} gateway_ip={} dns_server={}",
        host_fd, guest_network.guest_ip, guest_network.gateway_ip, guest_network.dns_server
    );
    let queues = NetworkFrameQueues::shared(DEFAULT_FRAME_QUEUE_CAPACITY);
    let frame_bridge = start_frame_stream_bridge(host_fd, queues.clone())?;
    let poll_handle = start_network_stack(
        queues.clone(),
        VirtioPollConfig {
            gateway_mac: guest_network.gateway_mac,
            guest_mac: guest_network.guest_mac,
            gateway_ipv4: guest_network.gateway_ip,
            guest_ipv4: guest_network.guest_ip,
            mtu: 1500,
        },
    )?;

    Ok(VirtioNetworkRuntime {
        queues,
        _frame_bridge: frame_bridge,
        poll_handle: Some(poll_handle),
    })
}

impl Drop for VirtioNetworkRuntime {
    fn drop(&mut self) {
        self.queues.begin_shutdown();
        if let Some(handle) = self.poll_handle.take() {
            let _ = handle.join();
        }
    }
}
