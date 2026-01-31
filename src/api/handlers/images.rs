//! Image management handlers.

use axum::{
    extract::{Path, State},
    Json,
};
use std::sync::Arc;

use crate::api::error::ApiError;
use crate::api::state::{
    mount_spec_to_host_mount, port_spec_to_mapping, resource_spec_to_vm_resources, ApiState,
};
use crate::agent::PullOptions;
use crate::api::types::{ImageInfo, ListImagesResponse, PullImageRequest, PullImageResponse};

/// GET /api/v1/sandboxes/:id/images - List images in a sandbox.
pub async fn list_images(
    State(state): State<Arc<ApiState>>,
    Path(sandbox_id): Path<String>,
) -> Result<Json<ListImagesResponse>, ApiError> {
    let entry = state.get_sandbox(&sandbox_id)?;

    // Check if sandbox is running, return empty list if not
    {
        let entry = entry.lock();
        if !entry.manager.is_running() {
            return Ok(Json(ListImagesResponse { images: Vec::new() }));
        }
    }

    // List images in blocking task
    let entry_clone = entry.clone();
    let images = tokio::task::spawn_blocking(move || {
        let entry = entry_clone.lock();
        let mut client = entry.manager.connect()?;
        client.list_images()
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    let images = images
        .into_iter()
        .map(|i| ImageInfo {
            reference: i.reference,
            digest: i.digest,
            size: i.size,
            architecture: i.architecture,
            os: i.os,
            layer_count: i.layer_count,
        })
        .collect();

    Ok(Json(ListImagesResponse { images }))
}

/// POST /api/v1/sandboxes/:id/images/pull - Pull an image.
pub async fn pull_image(
    State(state): State<Arc<ApiState>>,
    Path(sandbox_id): Path<String>,
    Json(req): Json<PullImageRequest>,
) -> Result<Json<PullImageResponse>, ApiError> {
    if req.image.is_empty() {
        return Err(ApiError::BadRequest(
            "image reference cannot be empty".into(),
        ));
    }

    let entry = state.get_sandbox(&sandbox_id)?;

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
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    // Pull image in blocking task
    let image = req.image.clone();
    let platform = req.platform.clone();
    let entry_clone = entry.clone();
    let image_info = tokio::task::spawn_blocking(move || {
        let entry = entry_clone.lock();
        let mut client = entry.manager.connect()?;
        let mut opts = PullOptions::new().use_registry_config(true);
        if let Some(p) = platform {
            opts = opts.platform(p);
        }
        client.pull(&image, opts)
    })
    .await?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(Json(PullImageResponse {
        image: ImageInfo {
            reference: image_info.reference,
            digest: image_info.digest,
            size: image_info.size,
            architecture: image_info.architecture,
            os: image_info.os,
            layer_count: image_info.layer_count,
        },
    }))
}
