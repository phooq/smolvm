//! smolvm guest agent.
//!
//! This agent runs inside smolvm VMs and handles:
//! - OCI image pulling via crane
//! - Layer extraction and storage management
//! - Overlay filesystem preparation for workloads
//! - Command execution with optional interactive/TTY support
//!
//! Communication is via vsock on port 6000.

use smolvm_protocol::{
    ports, AgentRequest, AgentResponse, ImageInfo, OverlayInfo, StorageStatus,
    PROTOCOL_VERSION,
};
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::process::{Child, Command, Stdio};
use tracing::{debug, error, info, warn};

mod storage;
mod vsock;

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("smolvm_agent=debug".parse().unwrap()),
        )
        .init();

    info!(version = env!("CARGO_PKG_VERSION"), "starting smolvm-agent");

    // Initialize storage
    if let Err(e) = storage::init() {
        error!(error = %e, "failed to initialize storage");
        std::process::exit(1);
    }

    // Start vsock server
    if let Err(e) = run_server() {
        error!(error = %e, "server error");
        std::process::exit(1);
    }
}

/// Run the vsock server.
fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let listener = vsock::listen(ports::AGENT_CONTROL)?;
    info!(port = ports::AGENT_CONTROL, "listening on vsock");

    loop {
        match listener.accept() {
            Ok(mut stream) => {
                info!("accepted connection");

                if let Err(e) = handle_connection(&mut stream) {
                    warn!(error = %e, "connection error");
                }
            }
            Err(e) => {
                warn!(error = %e, "accept error");
            }
        }
    }
}

/// Handle a single connection.
fn handle_connection(stream: &mut impl ReadWrite) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = vec![0u8; 64 * 1024]; // 64KB buffer

    loop {
        // Read length header
        let mut header = [0u8; 4];
        match stream.read_exact(&mut header) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                debug!("connection closed");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }

        let len = u32::from_be_bytes(header) as usize;
        if len > buf.len() {
            buf.resize(len, 0);
        }

        // Read payload
        stream.read_exact(&mut buf[..len])?;

        // Parse request
        let request: AgentRequest = match serde_json::from_slice(&buf[..len]) {
            Ok(req) => req,
            Err(e) => {
                warn!(error = %e, "invalid request");
                send_response(stream, &AgentResponse::Error {
                    message: format!("invalid request: {}", e),
                    code: Some("INVALID_REQUEST".to_string()),
                })?;
                continue;
            }
        };

        debug!(?request, "received request");

        // Check if this is an interactive run request
        if let AgentRequest::Run { interactive: true, .. } | AgentRequest::Run { tty: true, .. } = &request {
            // Handle interactive session
            handle_interactive_run(stream, request)?;
            continue;
        }

        // Handle regular request
        let response = handle_request(request);
        send_response(stream, &response)?;

        // Check for shutdown
        if matches!(response, AgentResponse::Ok { .. }) {
            if let AgentResponse::Ok { data: Some(ref d) } = response {
                if d.get("shutdown").and_then(|v| v.as_bool()) == Some(true) {
                    info!("shutdown requested");
                    return Ok(());
                }
            }
        }
    }
}

/// Handle a single non-interactive request.
fn handle_request(request: AgentRequest) -> AgentResponse {
    match request {
        AgentRequest::Ping => AgentResponse::Pong {
            version: PROTOCOL_VERSION,
        },

        AgentRequest::Pull { image, platform } => handle_pull(&image, platform.as_deref()),

        AgentRequest::Query { image } => handle_query(&image),

        AgentRequest::ListImages => handle_list_images(),

        AgentRequest::GarbageCollect { dry_run } => handle_gc(dry_run),

        AgentRequest::PrepareOverlay { image, workload_id } => {
            handle_prepare_overlay(&image, &workload_id)
        }

        AgentRequest::CleanupOverlay { workload_id } => handle_cleanup_overlay(&workload_id),

        AgentRequest::FormatStorage => handle_format_storage(),

        AgentRequest::StorageStatus => handle_storage_status(),

        AgentRequest::Shutdown => {
            info!("shutdown requested");
            AgentResponse::Ok {
                data: Some(serde_json::json!({"shutdown": true})),
            }
        }

        AgentRequest::Run {
            image,
            command,
            env,
            workdir,
            mounts,
            timeout_ms,
            interactive: false,
            tty: false,
        } => handle_run(&image, &command, &env, workdir.as_deref(), &mounts, timeout_ms),

        AgentRequest::Run { .. } => {
            // Interactive mode should be handled by handle_interactive_run
            AgentResponse::Error {
                message: "interactive mode not handled here".into(),
                code: Some("INTERNAL_ERROR".into()),
            }
        }

        AgentRequest::Stdin { .. } | AgentRequest::Resize { .. } => {
            AgentResponse::Error {
                message: "stdin/resize only valid during interactive session".into(),
                code: Some("INVALID_REQUEST".into()),
            }
        }
    }
}

/// Handle an interactive run session with streaming I/O.
fn handle_interactive_run(
    stream: &mut impl ReadWrite,
    request: AgentRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let (image, command, env, workdir, mounts, timeout_ms, tty) = match request {
        AgentRequest::Run {
            image,
            command,
            env,
            workdir,
            mounts,
            timeout_ms,
            tty,
            ..
        } => (image, command, env, workdir, mounts, timeout_ms, tty),
        _ => {
            send_response(stream, &AgentResponse::Error {
                message: "expected Run request".into(),
                code: Some("INVALID_REQUEST".into()),
            })?;
            return Ok(());
        }
    };

    info!(image = %image, command = ?command, tty = tty, "starting interactive run");

    // Prepare the overlay and get the rootfs path
    let rootfs = match storage::prepare_for_run(&image) {
        Ok(path) => path,
        Err(e) => {
            send_response(stream, &AgentResponse::Error {
                message: e.to_string(),
                code: Some("RUN_FAILED".into()),
            })?;
            return Ok(());
        }
    };

    // Setup volume mounts
    if let Err(e) = storage::setup_mounts(&rootfs, &mounts) {
        send_response(stream, &AgentResponse::Error {
            message: e.to_string(),
            code: Some("MOUNT_FAILED".into()),
        })?;
        return Ok(());
    }

    // Spawn the command
    let mut child = match spawn_interactive_command(&rootfs, &command, &env, workdir.as_deref(), tty) {
        Ok(child) => child,
        Err(e) => {
            send_response(stream, &AgentResponse::Error {
                message: e.to_string(),
                code: Some("SPAWN_FAILED".into()),
            })?;
            return Ok(());
        }
    };

    // Send Started response
    send_response(stream, &AgentResponse::Started)?;

    // Run the interactive I/O loop
    let exit_code = run_interactive_loop(stream, &mut child, timeout_ms)?;

    // Send Exited response
    send_response(stream, &AgentResponse::Exited { exit_code })?;

    Ok(())
}

/// Spawn a command for interactive execution.
fn spawn_interactive_command(
    rootfs: &str,
    command: &[String],
    env: &[(String, String)],
    workdir: Option<&str>,
    _tty: bool,
) -> Result<Child, Box<dyn std::error::Error>> {
    if command.is_empty() {
        return Err("empty command".into());
    }

    // Build chroot command
    let mut cmd = Command::new("chroot");
    cmd.arg(rootfs);

    // Add the actual command
    for arg in command {
        cmd.arg(arg);
    }

    // Set environment
    cmd.env_clear();
    cmd.env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
    cmd.env("HOME", "/root");
    cmd.env("TERM", "xterm-256color");
    for (k, v) in env {
        cmd.env(k, v);
    }

    // Set working directory
    if let Some(wd) = workdir {
        cmd.current_dir(format!("{}{}", rootfs, wd));
    }

    // Setup stdio for interactive mode
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // TODO: For TTY mode, allocate a PTY instead of pipes
    // This would use openpty() and connect the child to the PTY

    info!(command = ?command, "spawning interactive command");
    let child = cmd.spawn()?;

    Ok(child)
}

/// Run the interactive I/O loop.
fn run_interactive_loop(
    stream: &mut impl ReadWrite,
    child: &mut Child,
    timeout_ms: Option<u64>,
) -> Result<i32, Box<dyn std::error::Error>> {
    use std::io::Read as _;
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let deadline = timeout_ms.map(|ms| start + Duration::from_millis(ms));

    // Get handles to child's stdio
    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();
    let mut child_stdin = child.stdin.take();

    // Set non-blocking mode on stdout/stderr
    if let Some(ref stdout) = child_stdout {
        set_nonblocking(stdout.as_raw_fd());
    }
    if let Some(ref stderr) = child_stderr {
        set_nonblocking(stderr.as_raw_fd());
    }

    let mut stdout_buf = [0u8; 4096];
    let mut stderr_buf = [0u8; 4096];

    loop {
        // Check if child has exited
        match child.try_wait()? {
            Some(status) => {
                // Drain any remaining output
                if let Some(ref mut stdout) = child_stdout {
                    loop {
                        match stdout.read(&mut stdout_buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                send_response(stream, &AgentResponse::Stdout {
                                    data: stdout_buf[..n].to_vec(),
                                })?;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => break,
                        }
                    }
                }
                if let Some(ref mut stderr) = child_stderr {
                    loop {
                        match stderr.read(&mut stderr_buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                send_response(stream, &AgentResponse::Stderr {
                                    data: stderr_buf[..n].to_vec(),
                                })?;
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => break,
                        }
                    }
                }
                return Ok(status.code().unwrap_or(-1));
            }
            None => {}
        }

        // Check timeout
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                warn!("interactive command timed out");
                let _ = child.kill();
                let _ = child.wait();
                return Ok(124); // Timeout exit code
            }
        }

        // Read available stdout
        if let Some(ref mut stdout) = child_stdout {
            match stdout.read(&mut stdout_buf) {
                Ok(0) => {} // EOF
                Ok(n) => {
                    send_response(stream, &AgentResponse::Stdout {
                        data: stdout_buf[..n].to_vec(),
                    })?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    debug!(error = %e, "stdout read error");
                }
            }
        }

        // Read available stderr
        if let Some(ref mut stderr) = child_stderr {
            match stderr.read(&mut stderr_buf) {
                Ok(0) => {} // EOF
                Ok(n) => {
                    send_response(stream, &AgentResponse::Stderr {
                        data: stderr_buf[..n].to_vec(),
                    })?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    debug!(error = %e, "stderr read error");
                }
            }
        }

        // Check for incoming stdin data (non-blocking read from vsock)
        // This requires the stream to support non-blocking or using poll/select
        // For now, we use a simple polling approach with short timeout

        // Try to read a request (with timeout)
        if let Some(request) = try_read_request(stream)? {
            match request {
                AgentRequest::Stdin { data } => {
                    if let Some(ref mut stdin) = child_stdin {
                        let _ = stdin.write_all(&data);
                        let _ = stdin.flush();
                    }
                }
                AgentRequest::Resize { cols, rows } => {
                    // TODO: Implement PTY resize using TIOCSWINSZ
                    debug!(cols, rows, "resize requested (not implemented)");
                }
                _ => {
                    warn!("unexpected request during interactive session");
                }
            }
        }

        // Small sleep to prevent busy-waiting
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Try to read a request with a very short timeout.
fn try_read_request(stream: &mut impl ReadWrite) -> Result<Option<AgentRequest>, Box<dyn std::error::Error>> {
    // For now, use a simple non-blocking approach
    // In a production implementation, we'd use poll/select

    // This is a simplified version - we'll check if data is available
    // by trying to peek or using non-blocking read

    // For the initial implementation, we'll skip stdin forwarding
    // and just focus on output streaming
    Ok(None)
}

/// Set a file descriptor to non-blocking mode.
fn set_nonblocking(fd: i32) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

/// Handle command execution request (non-interactive).
fn handle_run(
    image: &str,
    command: &[String],
    env: &[(String, String)],
    workdir: Option<&str>,
    mounts: &[(String, String, bool)],
    timeout_ms: Option<u64>,
) -> AgentResponse {
    info!(image = %image, command = ?command, mounts = ?mounts, timeout_ms = ?timeout_ms, "running command");

    match storage::run_command(image, command, env, workdir, mounts, timeout_ms) {
        Ok(result) => AgentResponse::Completed {
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
        },
        Err(e) => AgentResponse::Error {
            message: e.to_string(),
            code: Some("RUN_FAILED".to_string()),
        },
    }
}

/// Handle image pull request.
fn handle_pull(image: &str, platform: Option<&str>) -> AgentResponse {
    info!(image = %image, ?platform, "pulling image");

    match storage::pull_image(image, platform) {
        Ok(info) => AgentResponse::Ok {
            data: Some(serde_json::to_value(info).unwrap()),
        },
        Err(e) => AgentResponse::Error {
            message: e.to_string(),
            code: Some("PULL_FAILED".to_string()),
        },
    }
}

/// Handle image query request.
fn handle_query(image: &str) -> AgentResponse {
    match storage::query_image(image) {
        Ok(Some(info)) => AgentResponse::Ok {
            data: Some(serde_json::to_value(info).unwrap()),
        },
        Ok(None) => AgentResponse::Error {
            message: format!("image not found: {}", image),
            code: Some("NOT_FOUND".to_string()),
        },
        Err(e) => AgentResponse::Error {
            message: e.to_string(),
            code: Some("QUERY_FAILED".to_string()),
        },
    }
}

/// Handle list images request.
fn handle_list_images() -> AgentResponse {
    match storage::list_images() {
        Ok(images) => AgentResponse::Ok {
            data: Some(serde_json::to_value(images).unwrap()),
        },
        Err(e) => AgentResponse::Error {
            message: e.to_string(),
            code: Some("LIST_FAILED".to_string()),
        },
    }
}

/// Handle garbage collection request.
fn handle_gc(dry_run: bool) -> AgentResponse {
    match storage::garbage_collect(dry_run) {
        Ok(freed) => AgentResponse::Ok {
            data: Some(serde_json::json!({
                "freed_bytes": freed,
                "dry_run": dry_run,
            })),
        },
        Err(e) => AgentResponse::Error {
            message: e.to_string(),
            code: Some("GC_FAILED".to_string()),
        },
    }
}

/// Handle overlay preparation request.
fn handle_prepare_overlay(image: &str, workload_id: &str) -> AgentResponse {
    info!(image = %image, workload_id = %workload_id, "preparing overlay");

    match storage::prepare_overlay(image, workload_id) {
        Ok(info) => AgentResponse::Ok {
            data: Some(serde_json::to_value(info).unwrap()),
        },
        Err(e) => AgentResponse::Error {
            message: e.to_string(),
            code: Some("OVERLAY_FAILED".to_string()),
        },
    }
}

/// Handle overlay cleanup request.
fn handle_cleanup_overlay(workload_id: &str) -> AgentResponse {
    info!(workload_id = %workload_id, "cleaning up overlay");

    match storage::cleanup_overlay(workload_id) {
        Ok(_) => AgentResponse::Ok { data: None },
        Err(e) => AgentResponse::Error {
            message: e.to_string(),
            code: Some("CLEANUP_FAILED".to_string()),
        },
    }
}

/// Handle storage format request.
fn handle_format_storage() -> AgentResponse {
    info!("formatting storage");

    match storage::format() {
        Ok(_) => AgentResponse::Ok { data: None },
        Err(e) => AgentResponse::Error {
            message: e.to_string(),
            code: Some("FORMAT_FAILED".to_string()),
        },
    }
}

/// Handle storage status request.
fn handle_storage_status() -> AgentResponse {
    match storage::status() {
        Ok(status) => AgentResponse::Ok {
            data: Some(serde_json::to_value(status).unwrap()),
        },
        Err(e) => AgentResponse::Error {
            message: e.to_string(),
            code: Some("STATUS_FAILED".to_string()),
        },
    }
}

/// Send a response to the client.
fn send_response(
    stream: &mut impl Write,
    response: &AgentResponse,
) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_vec(response)?;
    let len = json.len() as u32;

    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(&json)?;
    stream.flush()?;

    debug!(?response, "sent response");
    Ok(())
}

/// Trait for read+write streams.
trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}
