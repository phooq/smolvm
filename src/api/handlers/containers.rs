//! Container management handlers.

use axum::{
    extract::{Path, State},
    Json,
};
use std::sync::Arc;
use std::time::Duration;

use crate::api::error::ApiError;
use crate::api::state::{mount_spec_to_host_mount, port_spec_to_mapping, resource_spec_to_vm_resources, ApiState};
use crate::api::types::{
    ContainerExecRequest, ContainerInfo, CreateContainerRequest, DeleteContainerRequest,
    ExecResponse, ListContainersResponse, StopContainerRequest,
};

/// POST /api/v1/sandboxes/:id/containers - Create a container.
pub async fn create_container(
    State(state): State<Arc<ApiState>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<CreateContainerRequest>,
) -> Result<Json<ContainerInfo>, ApiError> {
    let entry = state.get_sandbox(&sandbox_id)?;

    // Ensure sandbox is running (blocking operation)
    {
        let entry_clone = entry.clone();
        tokio::task::spawn_blocking(move || {
            let entry = entry_clone.lock();
            let mounts_result: Result<Vec<_>, _> = entry
                .mounts
                .iter()
                .map(mount_spec_to_host_mount)
                .collect();
            let mounts = mounts_result?;
            let ports: Vec<_> = entry.ports.iter().map(port_spec_to_mapping).collect();
            let resources = resource_spec_to_vm_resources(&entry.resources);

            entry
                .manager
                .ensure_running_with_full_config(mounts, ports, resources)
        })
        .await?
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    // Prepare parameters
    let image = req.image.clone();
    let command = if req.command.is_empty() {
        vec!["sleep".to_string(), "infinity".to_string()]
    } else {
        req.command.clone()
    };
    let env: Vec<(String, String)> = req
        .env
        .iter()
        .map(|e| (e.name.clone(), e.value.clone()))
        .collect();
    let workdir = req.workdir.clone();
    let mounts: Vec<(String, String, bool)> = req
        .mounts
        .iter()
        .map(|m| (m.source.clone(), m.target.clone(), m.readonly))
        .collect();

    // Create container in blocking task
    let entry_clone = entry.clone();
    let container_info = tokio::task::spawn_blocking(move || {
        let entry = entry_clone.lock();
        let mut client = entry.manager.connect()?;
        client.create_container(&image, command, env, workdir, mounts)
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(ContainerInfo {
        id: container_info.id,
        image: container_info.image,
        state: container_info.state,
        created_at: container_info.created_at,
        command: container_info.command,
    }))
}

/// GET /api/v1/sandboxes/:id/containers - List containers.
pub async fn list_containers(
    State(state): State<Arc<ApiState>>,
    Path(sandbox_id): Path<String>,
) -> Result<Json<ListContainersResponse>, ApiError> {
    let entry = state.get_sandbox(&sandbox_id)?;

    // Check if sandbox is running, return empty list if not
    {
        let entry = entry.lock();
        if !entry.manager.is_running() {
            return Ok(Json(ListContainersResponse {
                containers: Vec::new(),
            }));
        }
    }

    // List containers in blocking task
    let entry_clone = entry.clone();
    let containers = tokio::task::spawn_blocking(move || {
        let entry = entry_clone.lock();
        let mut client = entry.manager.connect()?;
        client.list_containers()
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    let containers = containers
        .into_iter()
        .map(|c| ContainerInfo {
            id: c.id,
            image: c.image,
            state: c.state,
            created_at: c.created_at,
            command: c.command,
        })
        .collect();

    Ok(Json(ListContainersResponse { containers }))
}

/// POST /api/v1/sandboxes/:id/containers/:cid/start - Start a container.
pub async fn start_container(
    State(state): State<Arc<ApiState>>,
    Path((sandbox_id, container_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entry = state.get_sandbox(&sandbox_id)?;

    // Clone container_id for the response
    let container_id_response = container_id.clone();

    // Start container in blocking task
    let entry_clone = entry.clone();
    tokio::task::spawn_blocking(move || {
        let entry = entry_clone.lock();
        let mut client = entry.manager.connect()?;
        client.start_container(&container_id)
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "started": container_id_response
    })))
}

/// POST /api/v1/sandboxes/:id/containers/:cid/stop - Stop a container.
pub async fn stop_container(
    State(state): State<Arc<ApiState>>,
    Path((sandbox_id, container_id)): Path<(String, String)>,
    Json(req): Json<StopContainerRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entry = state.get_sandbox(&sandbox_id)?;

    let timeout_secs = req.timeout_secs;

    // Clone container_id for the response
    let container_id_response = container_id.clone();

    // Stop container in blocking task
    let entry_clone = entry.clone();
    tokio::task::spawn_blocking(move || {
        let entry = entry_clone.lock();
        let mut client = entry.manager.connect()?;
        client.stop_container(&container_id, timeout_secs)
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "stopped": container_id_response
    })))
}

/// DELETE /api/v1/sandboxes/:id/containers/:cid - Delete a container.
pub async fn delete_container(
    State(state): State<Arc<ApiState>>,
    Path((sandbox_id, container_id)): Path<(String, String)>,
    Json(req): Json<DeleteContainerRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entry = state.get_sandbox(&sandbox_id)?;

    let force = req.force;

    // Clone container_id for the response
    let container_id_response = container_id.clone();

    // Delete container in blocking task
    let entry_clone = entry.clone();
    tokio::task::spawn_blocking(move || {
        let entry = entry_clone.lock();
        let mut client = entry.manager.connect()?;
        client.delete_container(&container_id, force)
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(serde_json::json!({
        "deleted": container_id_response
    })))
}

/// POST /api/v1/sandboxes/:id/containers/:cid/exec - Execute in a container.
pub async fn exec_in_container(
    State(state): State<Arc<ApiState>>,
    Path((sandbox_id, container_id)): Path<(String, String)>,
    Json(req): Json<ContainerExecRequest>,
) -> Result<Json<ExecResponse>, ApiError> {
    if req.command.is_empty() {
        return Err(ApiError::BadRequest("command cannot be empty".into()));
    }

    let entry = state.get_sandbox(&sandbox_id)?;

    // Prepare parameters
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
        client.exec(&container_id, command, env, workdir, timeout)
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(ExecResponse {
        exit_code,
        stdout,
        stderr,
    }))
}
