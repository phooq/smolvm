//! smoltcp device adapter for the virtio-net backend.

use crate::network::virtio::queues::NetworkFrameQueues;
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// smoltcp `Device` backed by shared frame queues.
pub struct VirtioNetworkDevice {
    queues: Arc<NetworkFrameQueues>,
    mtu: usize,
    staged_guest_frame: Option<Vec<u8>>,
    /// Set when smoltcp emitted at least one frame for the guest.
    pub(crate) frames_emitted: AtomicBool,
}

/// RX token representing one guest ethernet frame.
pub struct DeviceRxToken {
    frame: Vec<u8>,
}

/// TX token representing one outgoing frame from smoltcp.
pub struct DeviceTxToken<'a> {
    device: &'a mut VirtioNetworkDevice,
}

impl VirtioNetworkDevice {
    /// Create a new device for the given queues and MTU.
    pub fn new(queues: Arc<NetworkFrameQueues>, mtu: usize) -> Self {
        Self {
            queues,
            mtu,
            staged_guest_frame: None,
            frames_emitted: AtomicBool::new(false),
        }
    }

    /// Stage one guest frame so the poll loop can inspect it before smoltcp consumes it.
    pub fn stage_next_frame(&mut self) -> Option<&[u8]> {
        if self.staged_guest_frame.is_none() {
            self.staged_guest_frame = self.queues.guest_to_host.pop();
        }

        self.staged_guest_frame.as_deref()
    }

    /// Drop the currently staged guest frame.
    pub fn drop_staged_frame(&mut self) {
        self.staged_guest_frame = None;
    }
}

impl phy::Device for VirtioNetworkDevice {
    type RxToken<'a> = DeviceRxToken;
    type TxToken<'a> = DeviceTxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.staged_guest_frame.take()?;
        Some((DeviceRxToken { frame }, DeviceTxToken { device: self }))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        if self.queues.host_to_guest.len() < self.queues.host_to_guest.capacity() {
            Some(DeviceTxToken { device: self })
        } else {
            None
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = self.mtu + 14;
        capabilities
    }
}

impl phy::RxToken for DeviceRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.frame)
    }
}

impl<'a> phy::TxToken for DeviceTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = vec![0u8; len];
        let result = f(&mut frame);
        if self.device.queues.host_to_guest.push(frame).is_ok() {
            self.device.frames_emitted.store(true, Ordering::Relaxed);
        } else {
            tracing::debug!("dropping outbound ethernet frame because the guest queue is full");
        }
        result
    }
}
