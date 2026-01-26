//! Command execution handlers.

use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use std::convert::Infallible;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::api::error::ApiError;
use crate::api::state::{
    mount_spec_to_host_mount, port_spec_to_mapping, resource_spec_to_vm_resources, ApiState,
};
use crate::api::types::{ExecRequest, ExecResponse, LogsQuery, RunRequest};

/// POST /api/v1/sandboxes/:id/exec - Execute a command in a sandbox.
///
/// This executes directly in the VM (not in a container).
pub async fn exec_command(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResponse>, ApiError> {
    if req.command.is_empty() {
        return Err(ApiError::BadRequest("command cannot be empty".into()));
    }

    let entry = state.get_sandbox(&id)?;

    // Ensure sandbox is running (blocking operation)
    {
        let entry_clone = entry.clone();
        tokio::task::spawn_blocking(move || {
            let entry = entry_clone.lock();
            let mounts_result: Result<Vec<_>, _> =
                entry.mounts.iter().map(mount_spec_to_host_mount).collect();
            let mounts = mounts_result?;
            let ports: Vec<_> = entry.ports.iter().map(port_spec_to_mapping).collect();
            let resources = resource_spec_to_vm_resources(&entry.resources);

            entry
                .manager
                .ensure_running_with_full_config(mounts, ports, resources)
        })
        .await?
        .map_err(|e| ApiError::BadRequest(format!("mount validation failed: {}", e)))?;
    }

    // Prepare execution parameters
    let command = req.command.clone();
    let env: Vec<(String, String)> = req
        .env
        .iter()
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect();
    let workdir = req.workdir.clone();
    let timeout = req.timeout_secs.map(Duration::from_secs);

    // Execute in blocking task
    let entry_clone = entry.clone();
    let (exit_code, stdout, stderr) = tokio::task::spawn_blocking(move || {
        let entry = entry_clone.lock();
        let mut client = entry.manager.connect()?;
        client.vm_exec(command, env, workdir, timeout)
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(ExecResponse {
        exit_code,
        stdout,
        stderr,
    }))
}

/// POST /api/v1/sandboxes/:id/run - Run a command in an image.
///
/// This creates a temporary overlay from the image and runs the command.
pub async fn run_command(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(req): Json<RunRequest>,
) -> Result<Json<ExecResponse>, ApiError> {
    if req.command.is_empty() {
        return Err(ApiError::BadRequest("command cannot be empty".into()));
    }

    let entry = state.get_sandbox(&id)?;

    // Ensure sandbox is running (blocking operation)
    {
        let entry_clone = entry.clone();
        tokio::task::spawn_blocking(move || {
            let entry = entry_clone.lock();
            let mounts_result: Result<Vec<_>, _> =
                entry.mounts.iter().map(mount_spec_to_host_mount).collect();
            let mounts = mounts_result?;
            let ports: Vec<_> = entry.ports.iter().map(port_spec_to_mapping).collect();
            let resources = resource_spec_to_vm_resources(&entry.resources);

            entry
                .manager
                .ensure_running_with_full_config(mounts, ports, resources)
        })
        .await?
        .map_err(|e| ApiError::BadRequest(format!("mount validation failed: {}", e)))?;
    }

    // Prepare execution parameters
    let image = req.image.clone();
    let command = req.command.clone();
    let env: Vec<(String, String)> = req
        .env
        .iter()
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect();
    let workdir = req.workdir.clone();
    let timeout = req.timeout_secs.map(Duration::from_secs);

    // Get mounts from sandbox config (converted to protocol format)
    // Tags are "smolvm0", "smolvm1", etc. based on mount index
    let mounts_config = {
        let entry = entry.lock();
        entry
            .mounts
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let tag = format!("smolvm{}", i);
                (tag, m.target.clone(), m.readonly)
            })
            .collect::<Vec<_>>()
    };

    // Execute in blocking task
    let entry_clone = entry.clone();
    let (exit_code, stdout, stderr) = tokio::task::spawn_blocking(move || {
        let entry = entry_clone.lock();
        let mut client = entry.manager.connect()?;
        client.run_with_mounts_and_timeout(&image, command, env, workdir, mounts_config, timeout)
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(ExecResponse {
        exit_code,
        stdout,
        stderr,
    }))
}

/// GET /api/v1/sandboxes/:id/logs - Stream sandbox console logs via SSE.
///
/// Query parameters:
/// - `follow`: If true, keep streaming new logs (like tail -f)
/// - `tail`: Number of lines to show from the end
pub async fn stream_logs(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let entry = state.get_sandbox(&id)?;

    // Get console log path
    let log_path: PathBuf = {
        let entry = entry.lock();
        entry
            .manager
            .console_log()
            .ok_or_else(|| ApiError::NotFound("console log not configured".into()))?
            .to_path_buf()
    };

    // Check if file exists (blocking check is acceptable here since it's fast)
    let path_check = log_path.clone();
    let exists = tokio::task::spawn_blocking(move || path_check.exists())
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    if !exists {
        return Err(ApiError::NotFound(format!(
            "log file not found: {}",
            log_path.display()
        )));
    }

    let follow = query.follow;
    let tail = query.tail;

    // For tail, read last N lines upfront using spawn_blocking with bounded memory
    let (initial_lines, start_pos) = if let Some(n) = tail {
        let path = log_path.clone();
        tokio::task::spawn_blocking(move || read_last_n_lines_bounded(&path, n))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
            .map_err(|e| ApiError::Internal(e.to_string()))?
    } else {
        (Vec::new(), 0)
    };

    // Create the SSE stream
    let stream = async_stream::stream! {
        // Emit initial tail lines first
        for line in initial_lines {
            yield Ok(Event::default().data(line));
        }

        if tail.is_some() && !follow {
            return;
        }

        // For following or full read, poll the file for new content
        let mut pos = if tail.is_some() { start_pos } else { 0 };
        let mut partial_line = String::new();

        loop {
            // Read new content in spawn_blocking
            let path = log_path.clone();
            let current_pos = pos;

            let result = tokio::task::spawn_blocking(move || {
                read_from_position(&path, current_pos)
            }).await;

            match result {
                Ok(Ok((new_data, new_pos))) => {
                    pos = new_pos;
                    if !new_data.is_empty() {
                        partial_line.push_str(&new_data);
                        // Yield complete lines
                        while let Some(newline_pos) = partial_line.find('\n') {
                            let line = partial_line[..newline_pos].trim_end_matches('\r').to_string();
                            partial_line = partial_line[newline_pos + 1..].to_string();
                            yield Ok(Event::default().data(line));
                        }
                    }
                }
                Ok(Err(e)) => {
                    yield Ok(Event::default().data(format!("error: {}", e)));
                    break;
                }
                Err(e) => {
                    yield Ok(Event::default().data(format!("error: {}", e)));
                    break;
                }
            }

            if !follow {
                // Yield any remaining partial line
                if !partial_line.is_empty() {
                    yield Ok(Event::default().data(partial_line.clone()));
                }
                break;
            }

            // Wait before polling again
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Read the last N lines from a file using a bounded ring buffer.
/// Returns (lines, file_position_at_end) for follow mode.
fn read_last_n_lines_bounded(
    path: &std::path::Path,
    n: usize,
) -> std::io::Result<(Vec<String>, u64)> {
    use std::collections::VecDeque;

    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    let file_len = metadata.len();

    let reader = BufReader::new(file);

    // Use a ring buffer to keep only the last N lines in memory
    let mut ring: VecDeque<String> = VecDeque::with_capacity(n + 1);

    for line in reader.lines() {
        let line = line?;
        if ring.len() == n {
            ring.pop_front();
        }
        ring.push_back(line);
    }

    Ok((ring.into_iter().collect(), file_len))
}

/// Read new content from a file starting at a given position.
fn read_from_position(path: &std::path::Path, pos: u64) -> std::io::Result<(String, u64)> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    let file_len = metadata.len();

    if pos >= file_len {
        // No new content
        return Ok((String::new(), pos));
    }

    file.seek(SeekFrom::Start(pos))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;

    Ok((buf, file_len))
}
