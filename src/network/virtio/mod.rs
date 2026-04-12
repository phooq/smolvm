//! Host-side virtio-net runtime.
//!
//! Context
//! =======
//!
//! This module is the host-side half of the new networking path:
//!
//! ```text
//! guest app
//!   -> guest kernel TCP/IP stack
//!   -> virtio-net device
//!   -> libkrun unix-stream bridge
//!   -> smolvm FrameStreamBridge
//!   -> shared frame queues
//!   -> smoltcp gateway/runtime
//!   -> host sockets / DNS forwarding / TCP relay
//!   -> external network
//! ```
//!
//! Main runtime components:
//!
//! ```text
//! VirtioNetworkRuntime
//! ├─ FrameStreamBridge
//! │  ├─ reader thread
//! │  └─ writer thread
//! ├─ Arc<NetworkFrameQueues>
//! │  ├─ guest_to_host
//! │  ├─ host_to_guest
//! │  ├─ guest_wake
//! │  ├─ host_wake
//! │  └─ relay_wake
//! └─ smolvm-net-poll thread
//!    ├─ VirtioNetworkDevice
//!    ├─ smoltcp Interface
//!    ├─ SocketSet
//!    └─ TcpRelayTable
//! ```
//!
//! Component roles:
//! - `FrameStreamBridge`: translates libkrun's Unix-stream frame protocol into
//!   queue operations
//! - `NetworkFrameQueues`: handoff boundary between threads
//! - `VirtioNetworkDevice`: adapts those queues to smoltcp's `phy::Device`
//! - poll thread: acts as the guest-visible gateway and protocol dispatcher
//! - `TcpRelayTable`: maps guest TCP flows onto host-side relay threads
//!
//! In Phase 1 this runtime is responsible for:
//! - exchanging raw Ethernet frames with libkrun
//! - presenting a gateway endpoint to the guest
//! - handling DNS through a gateway UDP socket and host UDP forwarding
//! - relaying guest TCP connections to host `TcpStream`s
//!
//! What is *not* here yet:
//! - launcher wiring and `krun_add_net_unixstream()` setup
//! - port publishing
//! - non-DNS UDP relay
//! - policy enforcement / DNS filtering
//! - TLS MITM or deeper packet rewriting
//!
//! So this module is the host data plane, but not yet the full user-visible
//! networking feature.

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
///
/// Ownership model:
/// - one runtime instance corresponds to one guest virtio NIC
/// - it owns the queue set shared by the worker threads
/// - it owns the libkrun Unix-stream bridge threads
/// - it owns the smoltcp poll thread
///
/// Dropping the runtime is the shutdown signal. `Drop` marks the shared queues
/// as shutting down, wakes blocked workers, and joins the poll thread.
pub struct VirtioNetworkRuntime {
    queues: std::sync::Arc<NetworkFrameQueues>,
    _frame_bridge: FrameStreamBridge,
    poll_handle: Option<JoinHandle<()>>,
}

/// Start the host-side virtio-net runtime for one guest NIC.
///
/// Inputs:
/// - `host_fd`: the host-side Unix stream fd that libkrun will use for this
///   guest NIC. The launcher eventually gets this from the libkrun
///   `krun_add_net_unixstream()` setup path.
/// - `guest_network`: the static guest/gateway addressing and MAC plan for this
///   NIC.
///
/// High-level flow:
///
/// ```text
/// start_virtio_network()
///   -> create shared frame queues + wake pipes
///   -> start frame reader/writer threads on the Unix stream
///   -> start the smoltcp poll thread
///   -> return a handle that owns the whole runtime
/// ```
///
/// Expanded startup picture:
///
/// ```text
/// host_fd from libkrun
///   -> FrameStreamBridge(host_fd)
///      -> reader thread
///      -> writer thread
///   -> NetworkFrameQueues
///   -> start_network_stack(...)
///      -> poll thread owns smoltcp Interface + sockets
///   -> VirtioNetworkRuntime returned to launcher
/// ```
///
/// Outcome:
/// - guest->host Ethernet frames start flowing into the queues
/// - host->guest Ethernet frames emitted by smoltcp are written back to libkrun
/// - the poll loop starts acting as the guest-visible gateway
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
    /// Shut down the worker threads in a bounded, cooperative way.
    ///
    /// The queue shutdown flag wakes the frame bridge and smoltcp poll loop so
    /// they can exit on their own. We only explicitly join the poll thread
    /// here because the frame bridge joins its own threads in its own `Drop`.
    fn drop(&mut self) {
        self.queues.begin_shutdown();
        if let Some(handle) = self.poll_handle.take() {
            let _ = handle.join();
        }
    }
}
