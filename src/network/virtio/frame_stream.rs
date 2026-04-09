//! libkrun unix-stream framing for the virtio-net backend.

use crate::network::virtio::queues::NetworkFrameQueues;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

const FRAME_HEADER_LEN: usize = 4;
const SOCKET_SENDBUF_BYTES: libc::c_int = 16 * 1024 * 1024;
const MAX_FRAME_LEN: usize = 64 * 1024;

/// Running libkrun unix-stream bridge for one virtio NIC.
pub struct FrameStreamBridge {
    control: UnixStream,
    queues: Arc<NetworkFrameQueues>,
    reader_handle: Option<JoinHandle<()>>,
    writer_handle: Option<JoinHandle<()>>,
}

/// Start the libkrun unix-stream reader and writer threads for one virtio NIC.
pub fn start_frame_stream_bridge(
    fd: RawFd,
    queues: Arc<NetworkFrameQueues>,
) -> io::Result<FrameStreamBridge> {
    // SAFETY: ownership of the provided host-side socket fd transfers here.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    set_socket_send_buffer(&stream)?;

    let control = stream.try_clone()?;
    let reader = stream.try_clone()?;
    let writer = stream;

    let reader_handle = thread::Builder::new()
        .name("smolvm-net-reader".into())
        .spawn({
            let queues = queues.clone();
            move || run_reader(reader, queues)
        })?;

    let writer_queues = queues.clone();
    let writer_handle = thread::Builder::new()
        .name("smolvm-net-writer".into())
        .spawn(move || run_writer(writer, writer_queues))?;

    Ok(FrameStreamBridge {
        control,
        queues,
        reader_handle: Some(reader_handle),
        writer_handle: Some(writer_handle),
    })
}

impl Drop for FrameStreamBridge {
    fn drop(&mut self) {
        self.queues.begin_shutdown();
        let _ = self.control.shutdown(Shutdown::Both);

        if let Some(handle) = self.reader_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.writer_handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_reader(mut reader: UnixStream, queues: Arc<NetworkFrameQueues>) {
    loop {
        match read_frame(&mut reader) {
            Ok(frame) => {
                if queues.guest_to_host.push(frame).is_ok() {
                    queues.guest_wake.wake();
                } else {
                    tracing::warn!("dropping guest ethernet frame because the host queue is full");
                }
            }
            Err(err) => {
                queues.begin_shutdown();
                tracing::debug!(error = %err, "virtio-net reader thread stopped");
                return;
            }
        }
    }
}

fn run_writer(mut writer: UnixStream, queues: Arc<NetworkFrameQueues>) {
    loop {
        if queues.is_shutting_down() && queues.host_to_guest.is_empty() {
            return;
        }
        match queues.host_wake.wait(None) {
            Ok(true) => queues.host_wake.drain(),
            Ok(false) => continue,
            Err(err) => {
                queues.begin_shutdown();
                tracing::debug!(error = %err, "virtio-net writer wake pipe failed");
                return;
            }
        }

        while let Some(frame) = queues.host_to_guest.pop() {
            if let Err(err) = write_frame(&mut writer, &frame) {
                queues.begin_shutdown();
                tracing::debug!(error = %err, "virtio-net writer thread stopped");
                return;
            }
        }
    }
}

/// Read one raw ethernet frame using libkrun's 4-byte big-endian length prefix.
pub(crate) fn read_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut header = [0u8; FRAME_HEADER_LEN];
    reader.read_exact(&mut header)?;
    let frame_len = u32::from_be_bytes(header) as usize;

    if frame_len == 0 || frame_len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid ethernet frame length: {frame_len}"),
        ));
    }

    let mut frame = vec![0u8; frame_len];
    reader.read_exact(&mut frame)?;
    Ok(frame)
}

/// Write one raw ethernet frame using libkrun's 4-byte big-endian length prefix.
pub(crate) fn write_frame<W: Write>(writer: &mut W, frame: &[u8]) -> io::Result<()> {
    if frame.is_empty() || frame.len() > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid ethernet frame length: {}", frame.len()),
        ));
    }

    let header = (frame.len() as u32).to_be_bytes();
    write_all(writer, &header)?;
    write_all(writer, frame)?;
    writer.flush()
}

fn write_all<W: Write>(writer: &mut W, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let written = writer.write(buf)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short write while sending ethernet frame",
            ));
        }
        buf = &buf[written..];
    }
    Ok(())
}

fn set_socket_send_buffer(stream: &UnixStream) -> io::Result<()> {
    let size = SOCKET_SENDBUF_BYTES;
    // SAFETY: the socket file descriptor is valid and points at a Unix stream.
    let result = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&size as *const libc::c_int).cast(),
            std::mem::size_of_val(&size) as libc::socklen_t,
        )
    };
    if result < 0 {
        tracing::warn!(
            error = %io::Error::last_os_error(),
            "failed to increase SO_SNDBUF for virtio-net unixstream"
        );
        return Ok(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PartialWriter {
        written: Vec<u8>,
        chunk_size: usize,
    }

    impl Write for PartialWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let take = buf.len().min(self.chunk_size);
            self.written.extend_from_slice(&buf[..take]);
            Ok(take)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_frame_handles_partial_writes() {
        let mut writer = PartialWriter {
            written: Vec::new(),
            chunk_size: 3,
        };
        write_frame(&mut writer, &[1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(writer.written[..4], [0, 0, 0, 6]);
        assert_eq!(writer.written[4..], [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn read_frame_decodes_length_prefix() {
        let mut input = std::io::Cursor::new(vec![0, 0, 0, 3, 7, 8, 9]);
        assert_eq!(read_frame(&mut input).unwrap(), vec![7, 8, 9]);
    }

    #[test]
    fn unix_stream_round_trip_multiple_frames() {
        let (mut left, mut right) = UnixStream::pair().unwrap();
        write_frame(&mut left, &[1, 2, 3]).unwrap();
        write_frame(&mut left, &[4, 5]).unwrap();

        assert_eq!(read_frame(&mut right).unwrap(), vec![1, 2, 3]);
        assert_eq!(read_frame(&mut right).unwrap(), vec![4, 5]);
    }
}
