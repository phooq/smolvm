//! JSON request and response types for the API.

use serde::{Deserialize, Serialize};

// ============================================================================
// Sandbox Types
// ============================================================================

/// Request to create a new sandbox.
#[derive(Debug, Deserialize)]
pub struct CreateSandboxRequest {
    /// Unique name for the sandbox.
    pub name: String,
    /// Host mounts to attach.
    #[serde(default)]
    pub mounts: Vec<MountSpec>,
    /// Port mappings (host:guest).
    #[serde(default)]
    pub ports: Vec<PortSpec>,
    /// VM resource configuration.
    #[serde(default)]
    pub resources: Option<ResourceSpec>,
}

/// Mount specification (for requests).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MountSpec {
    /// Host path to mount.
    pub source: String,
    /// Path inside the sandbox.
    pub target: String,
    /// Read-only mount.
    #[serde(default)]
    pub readonly: bool,
}

/// Mount information (for responses, includes virtiofs tag).
#[derive(Debug, Clone, Serialize)]
pub struct MountInfo {
    /// Virtiofs tag (e.g., "smolvm0"). Use this in container mounts.
    pub tag: String,
    /// Host path.
    pub source: String,
    /// Path inside the sandbox.
    pub target: String,
    /// Read-only mount.
    pub readonly: bool,
}

/// Port mapping specification.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PortSpec {
    /// Port on the host.
    pub host: u16,
    /// Port inside the sandbox.
    pub guest: u16,
}

/// VM resource specification.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResourceSpec {
    /// Number of vCPUs.
    #[serde(default)]
    pub cpus: Option<u8>,
    /// Memory in MiB.
    #[serde(default)]
    pub memory_mb: Option<u32>,
}

/// Sandbox status information.
#[derive(Debug, Serialize)]
pub struct SandboxInfo {
    /// Sandbox name.
    pub name: String,
    /// Current state.
    pub state: String,
    /// Process ID (if running).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    /// Configured mounts (with virtiofs tags for use in container mounts).
    pub mounts: Vec<MountInfo>,
    /// Configured ports.
    pub ports: Vec<PortSpec>,
    /// VM resources.
    pub resources: ResourceSpec,
}

/// List sandboxes response.
#[derive(Debug, Serialize)]
pub struct ListSandboxesResponse {
    /// List of sandboxes.
    pub sandboxes: Vec<SandboxInfo>,
}

// ============================================================================
// Exec Types
// ============================================================================

/// Request to execute a command in a sandbox.
#[derive(Debug, Deserialize)]
pub struct ExecRequest {
    /// Command and arguments.
    pub command: Vec<String>,
    /// Environment variables.
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Working directory.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Environment variable.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EnvVar {
    /// Variable name.
    pub name: String,
    /// Variable value.
    pub value: String,
}

/// Command execution result.
#[derive(Debug, Serialize)]
pub struct ExecResponse {
    /// Exit code.
    pub exit_code: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

/// Request to run a command in an image.
#[derive(Debug, Deserialize)]
pub struct RunRequest {
    /// Image to run in.
    pub image: String,
    /// Command and arguments.
    pub command: Vec<String>,
    /// Environment variables.
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Working directory.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

// ============================================================================
// Container Types
// ============================================================================

/// Request to create a container.
#[derive(Debug, Deserialize)]
pub struct CreateContainerRequest {
    /// Image to use.
    pub image: String,
    /// Command and arguments.
    #[serde(default)]
    pub command: Vec<String>,
    /// Environment variables.
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Working directory.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Volume mounts.
    #[serde(default)]
    pub mounts: Vec<ContainerMountSpec>,
}

/// Container mount specification.
///
/// Note: The `source` field is the virtiofs tag, which corresponds to
/// host mounts configured on the sandbox. Tags are assigned in order:
/// `smolvm0`, `smolvm1`, etc. based on the sandbox's mount configuration.
/// Use `GET /api/v1/sandboxes/:id` to see the tag-to-path mapping.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContainerMountSpec {
    /// Virtiofs tag (e.g., "smolvm0", "smolvm1").
    /// These correspond to sandbox mounts in order.
    pub source: String,
    /// Target path in container.
    pub target: String,
    /// Read-only mount.
    #[serde(default)]
    pub readonly: bool,
}

/// Container information.
#[derive(Debug, Serialize)]
pub struct ContainerInfo {
    /// Container ID.
    pub id: String,
    /// Image.
    pub image: String,
    /// State (created, running, stopped).
    pub state: String,
    /// Creation timestamp.
    pub created_at: u64,
    /// Command.
    pub command: Vec<String>,
}

/// List containers response.
#[derive(Debug, Serialize)]
pub struct ListContainersResponse {
    /// List of containers.
    pub containers: Vec<ContainerInfo>,
}

/// Request to exec in a container.
#[derive(Debug, Deserialize)]
pub struct ContainerExecRequest {
    /// Command and arguments.
    pub command: Vec<String>,
    /// Environment variables.
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// Working directory.
    #[serde(default)]
    pub workdir: Option<String>,
    /// Timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Request to stop a container.
#[derive(Debug, Deserialize)]
pub struct StopContainerRequest {
    /// Timeout before force kill (seconds).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Request to delete a container.
#[derive(Debug, Deserialize)]
pub struct DeleteContainerRequest {
    /// Force delete even if running.
    #[serde(default)]
    pub force: bool,
}

// ============================================================================
// Image Types
// ============================================================================

/// Image information.
#[derive(Debug, Serialize)]
pub struct ImageInfo {
    /// Image reference.
    pub reference: String,
    /// Image digest.
    pub digest: String,
    /// Size in bytes.
    pub size: u64,
    /// Architecture.
    pub architecture: String,
    /// OS.
    pub os: String,
    /// Number of layers.
    pub layer_count: usize,
}

/// List images response.
#[derive(Debug, Serialize)]
pub struct ListImagesResponse {
    /// List of images.
    pub images: Vec<ImageInfo>,
}

/// Request to pull an image.
#[derive(Debug, Deserialize)]
pub struct PullImageRequest {
    /// Image reference.
    pub image: String,
    /// Platform (e.g., "linux/arm64").
    #[serde(default)]
    pub platform: Option<String>,
}

/// Pull image response.
#[derive(Debug, Serialize)]
pub struct PullImageResponse {
    /// Information about the pulled image.
    pub image: ImageInfo,
}

// ============================================================================
// Logs Types
// ============================================================================

/// Query parameters for logs endpoint.
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    /// If true, follow the logs (like tail -f). Default: false.
    #[serde(default)]
    pub follow: bool,
    /// Number of lines to show from the end (like tail -n). Default: all.
    #[serde(default)]
    pub tail: Option<usize>,
}

// ============================================================================
// Health Types
// ============================================================================

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Health status (e.g., "ok").
    pub status: &'static str,
    /// Server version.
    pub version: &'static str,
}

