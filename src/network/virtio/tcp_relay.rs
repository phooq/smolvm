//! TCP relay support for the virtio-net backend.

use crate::network::virtio::queues::WakePipe;
use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::wire::IpListenEndpoint;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const TCP_RX_BUFFER_BYTES: usize = 64 * 1024;
const TCP_TX_BUFFER_BYTES: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 256;
const CHANNEL_CAPACITY: usize = 32;
const RELAY_BUFFER_BYTES: usize = 16 * 1024;
const CLOSE_RETRY_LIMIT: u16 = 64;
const PROXY_IDLE_SLEEP: Duration = Duration::from_millis(10);

/// Track all active guest TCP connections bridged through host sockets.
pub struct TcpRelayTable {
    connections: HashMap<SocketHandle, TrackedConnection>,
    connection_keys: HashSet<(SocketAddr, SocketAddr)>,
    max_connections: usize,
}

/// Newly established guest connection ready for a host relay thread.
pub struct NewTcpConnection {
    /// Destination originally requested by the guest.
    pub destination: SocketAddr,
    /// Guest-to-host payloads read from the smoltcp socket.
    pub from_smoltcp: Receiver<Vec<u8>>,
    /// Host-to-guest payloads written back into the smoltcp socket.
    pub to_smoltcp: SyncSender<Vec<u8>>,
    /// Shared relay exit state.
    pub exit_state: RelayExitState,
}

#[derive(Debug)]
struct TrackedConnection {
    source: SocketAddr,
    destination: SocketAddr,
    to_proxy: SyncSender<Vec<u8>>,
    from_proxy: Receiver<Vec<u8>>,
    pending_proxy_endpoints: Option<PendingProxyEndpoints>,
    relay_spawned: bool,
    buffered_proxy_data: Option<(Vec<u8>, usize)>,
    close_attempts: u16,
    exit_state: RelayExitState,
}

#[derive(Debug)]
struct PendingProxyEndpoints {
    from_smoltcp: Receiver<Vec<u8>>,
    to_smoltcp: SyncSender<Vec<u8>>,
}

/// Host relay termination state shared between the poll loop and the relay thread.
#[derive(Clone, Debug)]
pub struct RelayExitState {
    inner: Arc<AtomicU8>,
}

/// How a host TCP relay thread terminated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RelayExitMode {
    /// Relay thread is still running.
    Running = 0,
    /// Remote side closed normally; send FIN toward the guest.
    Graceful = 1,
    /// Remote connect or I/O failed; abort the guest TCP socket.
    Abort = 2,
}

impl RelayExitState {
    fn new() -> Self {
        Self {
            inner: Arc::new(AtomicU8::new(RelayExitMode::Running as u8)),
        }
    }

    fn load(&self) -> RelayExitMode {
        match self.inner.load(Ordering::Relaxed) {
            1 => RelayExitMode::Graceful,
            2 => RelayExitMode::Abort,
            _ => RelayExitMode::Running,
        }
    }

    fn store(&self, mode: RelayExitMode) {
        self.inner.store(mode as u8, Ordering::Relaxed);
    }
}

impl TcpRelayTable {
    /// Create a new relay table.
    pub fn new(max_connections: Option<usize>) -> Self {
        Self {
            connections: HashMap::new(),
            connection_keys: HashSet::new(),
            max_connections: max_connections.unwrap_or(MAX_CONNECTIONS),
        }
    }

    /// Whether a relay socket already exists for the same guest source and destination.
    pub fn has_socket_for(&self, source: &SocketAddr, destination: &SocketAddr) -> bool {
        self.connection_keys.contains(&(*source, *destination))
    }

    /// Create a smoltcp TCP socket for a guest SYN.
    pub fn create_tcp_socket(
        &mut self,
        source: SocketAddr,
        destination: SocketAddr,
        sockets: &mut SocketSet<'_>,
    ) -> bool {
        if self.connections.len() >= self.max_connections {
            tracing::warn!("dropping TCP connection because the relay table is full");
            return false;
        }

        let rx_buffer = tcp::SocketBuffer::new(vec![0u8; TCP_RX_BUFFER_BYTES]);
        let tx_buffer = tcp::SocketBuffer::new(vec![0u8; TCP_TX_BUFFER_BYTES]);
        let mut socket = tcp::Socket::new(rx_buffer, tx_buffer);
        let std::net::IpAddr::V4(destination_ip) = destination.ip() else {
            return false;
        };

        let listen_endpoint = IpListenEndpoint {
            addr: Some(destination_ip.into()),
            port: destination.port(),
        };
        if socket.listen(listen_endpoint).is_err() {
            return false;
        }

        let handle = sockets.add(socket);

        let (to_proxy_tx, to_proxy_rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let (from_proxy_tx, from_proxy_rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let exit_state = RelayExitState::new();

        self.connection_keys.insert((source, destination));
        self.connections.insert(
            handle,
            TrackedConnection {
                source,
                destination,
                to_proxy: to_proxy_tx,
                from_proxy: from_proxy_rx,
                pending_proxy_endpoints: Some(PendingProxyEndpoints {
                    from_smoltcp: to_proxy_rx,
                    to_smoltcp: from_proxy_tx,
                }),
                relay_spawned: false,
                buffered_proxy_data: None,
                close_attempts: 0,
                exit_state,
            },
        );

        true
    }

    /// Relay TCP payloads between smoltcp sockets and host relay threads.
    pub fn relay_data(&mut self, sockets: &mut SocketSet<'_>) {
        let mut read_buffer = [0u8; RELAY_BUFFER_BYTES];

        for (&handle, connection) in &mut self.connections {
            if !connection.relay_spawned {
                continue;
            }

            let socket = sockets.get_mut::<tcp::Socket>(handle);

            match connection.exit_state.load() {
                RelayExitMode::Abort => {
                    socket.abort();
                    continue;
                }
                RelayExitMode::Graceful => {
                    flush_proxy_data(socket, connection);
                    if connection.buffered_proxy_data.is_none() {
                        socket.close();
                    } else {
                        connection.close_attempts += 1;
                        if connection.close_attempts >= CLOSE_RETRY_LIMIT {
                            socket.abort();
                        }
                    }
                    continue;
                }
                RelayExitMode::Running => {}
            }

            while socket.can_recv() {
                match socket.recv_slice(&mut read_buffer) {
                    Ok(bytes_read) if bytes_read > 0 => {
                        let payload = read_buffer[..bytes_read].to_vec();
                        if connection.to_proxy.try_send(payload).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }

            flush_proxy_data(socket, connection);
        }
    }

    /// Collect connections that reached ESTABLISHED and need a host relay thread.
    pub fn take_new_connections(&mut self, sockets: &mut SocketSet<'_>) -> Vec<NewTcpConnection> {
        let mut new_connections = Vec::new();

        for (&handle, connection) in &mut self.connections {
            if connection.relay_spawned {
                continue;
            }

            let socket = sockets.get::<tcp::Socket>(handle);
            if socket.state() == tcp::State::Established {
                connection.relay_spawned = true;

                if let Some(endpoints) = connection.pending_proxy_endpoints.take() {
                    new_connections.push(NewTcpConnection {
                        destination: connection.destination,
                        from_smoltcp: endpoints.from_smoltcp,
                        to_smoltcp: endpoints.to_smoltcp,
                        exit_state: connection.exit_state.clone(),
                    });
                }
            }
        }

        new_connections
    }

    /// Remove closed sockets and drop their relay endpoints.
    pub fn cleanup_closed(&mut self, sockets: &mut SocketSet<'_>) {
        let keys = &mut self.connection_keys;
        self.connections.retain(|&handle, connection| {
            let socket = sockets.get::<tcp::Socket>(handle);
            if socket.state() == tcp::State::Closed {
                keys.remove(&(connection.source, connection.destination));
                sockets.remove(handle);
                false
            } else {
                true
            }
        });
    }
}

/// Spawn one host TCP relay thread for an established guest connection.
pub fn spawn_tcp_relay(
    destination: SocketAddr,
    from_smoltcp: Receiver<Vec<u8>>,
    to_smoltcp: SyncSender<Vec<u8>>,
    relay_wake: Arc<WakePipe>,
    exit_state: RelayExitState,
) {
    let thread_name = format!("smolvm-tcp-{}", destination.port());
    eprintln!(
        "virtio-net: spawning host TCP relay thread destination={} thread={}",
        destination, thread_name
    );
    let _ = thread::Builder::new().name(thread_name).spawn(move || {
        run_tcp_relay(
            destination,
            from_smoltcp,
            to_smoltcp,
            relay_wake,
            exit_state,
        )
    });
}

fn run_tcp_relay(
    destination: SocketAddr,
    from_smoltcp: Receiver<Vec<u8>>,
    to_smoltcp: SyncSender<Vec<u8>>,
    relay_wake: Arc<WakePipe>,
    exit_state: RelayExitState,
) {
    eprintln!(
        "virtio-net: host TCP relay thread started destination={}",
        destination
    );
    match tcp_relay_loop(destination, from_smoltcp, to_smoltcp, relay_wake) {
        Ok(mode) => {
            eprintln!(
                "virtio-net: host TCP relay thread exited destination={} mode={:?}",
                destination, mode
            );
            exit_state.store(mode)
        }
        Err(err) => {
            eprintln!(
                "virtio-net: host TCP relay failed destination={} error={}",
                destination, err
            );
            exit_state.store(RelayExitMode::Abort);
        }
    }
}

fn tcp_relay_loop(
    destination: SocketAddr,
    from_smoltcp: Receiver<Vec<u8>>,
    to_smoltcp: SyncSender<Vec<u8>>,
    relay_wake: Arc<WakePipe>,
) -> io::Result<RelayExitMode> {
    eprintln!(
        "virtio-net: connecting host TCP relay socket destination={}",
        destination
    );
    let mut stream = TcpStream::connect(destination)?;
    stream.set_nonblocking(true)?;
    eprintln!(
        "virtio-net: host TCP relay socket connected destination={}",
        destination
    );

    let mut guest_write_closed = false;
    let mut read_buffer = [0u8; RELAY_BUFFER_BYTES];

    loop {
        let mut did_work = false;

        loop {
            match from_smoltcp.try_recv() {
                Ok(payload) => {
                    stream.write_all(&payload)?;
                    did_work = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !guest_write_closed {
                        let _ = stream.shutdown(Shutdown::Write);
                        guest_write_closed = true;
                    }
                    break;
                }
            }
        }

        match stream.read(&mut read_buffer) {
            Ok(0) => return Ok(RelayExitMode::Graceful),
            Ok(bytes_read) => {
                if to_smoltcp.send(read_buffer[..bytes_read].to_vec()).is_err() {
                    return Ok(RelayExitMode::Graceful);
                }
                relay_wake.wake();
                did_work = true;
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(err),
        }

        if !did_work {
            thread::sleep(PROXY_IDLE_SLEEP);
        }
    }
}

fn flush_proxy_data(socket: &mut tcp::Socket<'_>, connection: &mut TrackedConnection) {
    if let Some((data, offset)) = &mut connection.buffered_proxy_data {
        if socket.can_send() {
            match socket.send_slice(&data[*offset..]) {
                Ok(written) => {
                    *offset += written;
                    if *offset >= data.len() {
                        connection.buffered_proxy_data = None;
                    }
                }
                Err(_) => return,
            }
        } else {
            return;
        }
    }

    while connection.buffered_proxy_data.is_none() {
        match connection.from_proxy.try_recv() {
            Ok(payload) => {
                if socket.can_send() {
                    match socket.send_slice(&payload) {
                        Ok(written) if written < payload.len() => {
                            connection.buffered_proxy_data = Some((payload, written));
                        }
                        Err(_) => {
                            connection.buffered_proxy_data = Some((payload, 0));
                        }
                        _ => {}
                    }
                } else {
                    connection.buffered_proxy_data = Some((payload, 0));
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}
