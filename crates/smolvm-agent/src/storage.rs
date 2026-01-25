//! Storage management for the helper daemon.
//!
//! This module handles:
//! - Storage disk initialization and formatting
//! - OCI image pulling via crane
//! - Layer extraction and deduplication
//! - Overlay filesystem management
//! - Container execution via crun OCI runtime

use crate::crun::CrunCommand;
use crate::oci::{generate_container_id, OciSpec};
use crate::paths;
use crate::process::{wait_with_timeout_and_cleanup, WaitResult, TIMEOUT_EXIT_CODE};
use smolvm_protocol::{ImageInfo, OverlayInfo, StorageStatus};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::{debug, info, warn};

/// Storage root path (where the ext4 disk is mounted).
const STORAGE_ROOT: &str = "/storage";

/// Directory structure within storage.
const LAYERS_DIR: &str = "layers";
const CONFIGS_DIR: &str = "configs";
const MANIFESTS_DIR: &str = "manifests";
const OVERLAYS_DIR: &str = "overlays";

/// Error type for storage operations.
#[derive(Debug)]
pub struct StorageError(pub(crate) String);

impl StorageError {
    /// Create a new storage error with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        StorageError(message.into())
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StorageError {}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        StorageError(e.to_string())
    }
}

type Result<T> = std::result::Result<T, StorageError>;

/// Initialize storage directories.
///
/// This function ensures all required storage directories exist and are accessible.
/// Returns early (successfully) if storage hasn't been formatted yet.
pub fn init() -> Result<()> {
    let root = Path::new(STORAGE_ROOT);

    // Check if storage root exists or can be created
    if !root.exists() {
        info!(path = %root.display(), "creating storage root directory");
        std::fs::create_dir_all(root).map_err(|e| {
            StorageError::new(format!(
                "failed to create storage root '{}': {} (check permissions and disk space)",
                root.display(),
                e
            ))
        })?;
    }

    // Verify storage root is accessible
    if let Err(e) = std::fs::read_dir(root) {
        return Err(StorageError::new(format!(
            "storage root '{}' exists but is not accessible: {} (check permissions)",
            root.display(),
            e
        )));
    }

    // Check for marker file to see if formatted
    let marker = root.join(".smolvm_formatted");
    if !marker.exists() {
        info!(path = %root.display(), "storage not formatted, waiting for format request");
        return Ok(());
    }

    // Create directory structure with detailed error reporting
    let required_dirs = [
        (LAYERS_DIR, "OCI image layers"),
        (CONFIGS_DIR, "image configurations"),
        (MANIFESTS_DIR, "image manifests"),
        (OVERLAYS_DIR, "overlay filesystems"),
    ];

    let mut created_count = 0;
    for (dir, description) in &required_dirs {
        let path = root.join(dir);
        if !path.exists() {
            std::fs::create_dir_all(&path).map_err(|e| {
                StorageError::new(format!(
                    "failed to create {} directory '{}': {}",
                    description,
                    path.display(),
                    e
                ))
            })?;
            debug!(path = %path.display(), description = %description, "created directory");
            created_count += 1;
        }
    }

    // Create container runtime directories
    let container_dirs = [
        (paths::CONTAINERS_RUN_DIR, "container runtime state"),
        (paths::CONTAINERS_LOGS_DIR, "container logs"),
        (paths::CONTAINERS_EXIT_DIR, "container exit codes"),
    ];

    for (dir, description) in &container_dirs {
        let path = Path::new(dir);
        if !path.exists() {
            std::fs::create_dir_all(path).map_err(|e| {
                StorageError::new(format!(
                    "failed to create {} directory '{}': {}",
                    description,
                    path.display(),
                    e
                ))
            })?;
            debug!(path = %path.display(), description = %description, "created directory");
            created_count += 1;
        }
    }

    info!(
        path = %root.display(),
        dirs_created = created_count,
        "storage initialized"
    );
    Ok(())
}

/// Format the storage disk.
///
/// Creates all required directories and writes the format marker file.
/// If directories already exist, they are left as-is.
pub fn format() -> Result<()> {
    let root = Path::new(STORAGE_ROOT);

    // Ensure storage root exists
    if !root.exists() {
        std::fs::create_dir_all(root).map_err(|e| {
            StorageError::new(format!(
                "failed to create storage root '{}': {}",
                root.display(),
                e
            ))
        })?;
    }

    // Create all storage directories
    let all_dirs = [
        (root.join(LAYERS_DIR), "layers"),
        (root.join(CONFIGS_DIR), "configs"),
        (root.join(MANIFESTS_DIR), "manifests"),
        (root.join(OVERLAYS_DIR), "overlays"),
        (PathBuf::from(paths::CONTAINERS_RUN_DIR), "container run"),
        (PathBuf::from(paths::CONTAINERS_LOGS_DIR), "container logs"),
        (PathBuf::from(paths::CONTAINERS_EXIT_DIR), "container exit"),
    ];

    for (path, name) in &all_dirs {
        std::fs::create_dir_all(path).map_err(|e| {
            StorageError::new(format!(
                "failed to create {} directory '{}': {}",
                name,
                path.display(),
                e
            ))
        })?;
    }

    // Create marker file
    let marker = root.join(".smolvm_formatted");
    std::fs::write(&marker, "1").map_err(|e| {
        StorageError::new(format!(
            "failed to write format marker '{}': {}",
            marker.display(),
            e
        ))
    })?;

    info!(path = %root.display(), "storage formatted");
    Ok(())
}

/// Get storage status.
pub fn status() -> Result<StorageStatus> {
    let root = Path::new(STORAGE_ROOT);
    let marker = root.join(".smolvm_formatted");

    let ready = marker.exists();

    // Get disk usage (simplified)
    let (total_bytes, used_bytes) = get_disk_usage(root)?;

    // Count layers and images
    let layer_count = count_entries(&root.join(LAYERS_DIR))?;
    let image_count = count_entries(&root.join(MANIFESTS_DIR))?;

    Ok(StorageStatus {
        ready,
        total_bytes,
        used_bytes,
        layer_count,
        image_count,
    })
}

/// Pull an OCI image using crane.
pub fn pull_image(image: &str, platform: Option<&str>) -> Result<ImageInfo> {
    // Validate image reference before any operations
    crate::oci::validate_image_reference(image).map_err(StorageError)?;

    // Check if already cached - skip network call entirely
    if let Ok(Some(info)) = query_image(image) {
        debug!(image = %image, "image already cached, skipping pull");
        return Ok(info);
    }

    let root = Path::new(STORAGE_ROOT);

    // Determine platform - default to current architecture
    let platform = platform.or({
        #[cfg(target_arch = "aarch64")]
        {
            Some("linux/arm64")
        }
        #[cfg(target_arch = "x86_64")]
        {
            Some("linux/amd64")
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            None
        }
    });

    // Get manifest with platform specified
    info!(image = %image, platform = ?platform, "fetching manifest");
    let manifest = crane_manifest(image, platform)?;

    // Parse manifest to get config and layers
    let manifest_json: serde_json::Value =
        serde_json::from_str(&manifest).map_err(|e| StorageError(e.to_string()))?;

    // Handle manifest list (multi-arch) - this shouldn't happen with --platform but just in case
    let config_digest = if manifest_json.get("config").is_some() {
        manifest_json["config"]["digest"]
            .as_str()
            .ok_or_else(|| StorageError("missing config digest".into()))?
    } else if manifest_json.get("manifests").is_some() {
        // This is a manifest list, need to fetch platform-specific manifest
        return Err(StorageError(format!(
            "got manifest list instead of image manifest - platform may not be available. \
             manifests: {:?}",
            manifest_json["manifests"].as_array().map(|arr| arr
                .iter()
                .filter_map(|m| m["platform"]["architecture"].as_str())
                .collect::<Vec<_>>())
        )));
    } else {
        return Err(StorageError("unknown manifest format".into()));
    };

    let layers: Vec<String> = manifest_json["layers"]
        .as_array()
        .ok_or_else(|| StorageError("missing layers".into()))?
        .iter()
        .filter_map(|l| l["digest"].as_str().map(String::from))
        .collect();

    // Save manifest
    let manifest_path = root
        .join(MANIFESTS_DIR)
        .join(sanitize_image_name(image) + ".json");
    std::fs::write(&manifest_path, &manifest)?;

    // Fetch and save config
    let config = crane_config(image, platform)?;
    let config_id = config_digest
        .strip_prefix("sha256:")
        .unwrap_or(config_digest);
    let config_path = root.join(CONFIGS_DIR).join(format!("{}.json", config_id));
    std::fs::write(&config_path, &config)?;

    // Parse config for metadata
    let config_json: serde_json::Value =
        serde_json::from_str(&config).map_err(|e| StorageError(e.to_string()))?;

    // Extract layers
    let mut total_size = 0u64;
    for (i, layer_digest) in layers.iter().enumerate() {
        let layer_id = layer_digest.strip_prefix("sha256:").unwrap_or(layer_digest);
        let layer_dir = root.join(LAYERS_DIR).join(layer_id);

        if layer_dir.exists() {
            info!(layer = %layer_id, "layer already cached");
            continue;
        }

        info!(
            layer = %layer_id,
            progress = format!("{}/{}", i + 1, layers.len()),
            "extracting layer"
        );

        std::fs::create_dir_all(&layer_dir)?;

        // Stream layer directly to tar extraction
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "crane blob '{}@{}' {} | tar -xzf - -C '{}'",
                image,
                layer_digest,
                platform
                    .map(|p| format!("--platform={}", p))
                    .unwrap_or_default(),
                layer_dir.display()
            ))
            .status()?;

        if !status.success() {
            // Clean up failed layer
            let _ = std::fs::remove_dir_all(&layer_dir);
            return Err(StorageError(format!(
                "failed to extract layer {}",
                layer_digest
            )));
        }

        // Get layer size
        if let Ok(size) = dir_size(&layer_dir) {
            total_size += size;
        }
    }

    // Build ImageInfo
    let architecture = config_json["architecture"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let os = config_json["os"].as_str().unwrap_or("linux").to_string();
    let created = config_json["created"].as_str().map(String::from);

    Ok(ImageInfo {
        reference: image.to_string(),
        digest: config_digest.to_string(),
        size: total_size,
        created,
        architecture,
        os,
        layer_count: layers.len(),
        layers,
    })
}

/// Query if an image exists locally.
pub fn query_image(image: &str) -> Result<Option<ImageInfo>> {
    let root = Path::new(STORAGE_ROOT);
    let manifest_path = root
        .join(MANIFESTS_DIR)
        .join(sanitize_image_name(image) + ".json");

    if !manifest_path.exists() {
        return Ok(None);
    }

    // Read and parse manifest
    let manifest = std::fs::read_to_string(&manifest_path)?;
    let manifest_json: serde_json::Value =
        serde_json::from_str(&manifest).map_err(|e| StorageError(e.to_string()))?;

    let config_digest = manifest_json["config"]["digest"]
        .as_str()
        .ok_or_else(|| StorageError("missing config digest".into()))?;

    let layers: Vec<String> = manifest_json["layers"]
        .as_array()
        .ok_or_else(|| StorageError("missing layers".into()))?
        .iter()
        .filter_map(|l| l["digest"].as_str().map(String::from))
        .collect();

    // Read config
    let config_id = config_digest
        .strip_prefix("sha256:")
        .unwrap_or(config_digest);
    let config_path = root.join(CONFIGS_DIR).join(format!("{}.json", config_id));
    let config = std::fs::read_to_string(&config_path)?;
    let config_json: serde_json::Value =
        serde_json::from_str(&config).map_err(|e| StorageError(e.to_string()))?;

    let architecture = config_json["architecture"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let os = config_json["os"].as_str().unwrap_or("linux").to_string();
    let created = config_json["created"].as_str().map(String::from);

    // Calculate total size
    let mut total_size = 0u64;
    for layer_digest in &layers {
        let layer_id = layer_digest.strip_prefix("sha256:").unwrap_or(layer_digest);
        let layer_dir = root.join(LAYERS_DIR).join(layer_id);
        if let Ok(size) = dir_size(&layer_dir) {
            total_size += size;
        }
    }

    Ok(Some(ImageInfo {
        reference: image.to_string(),
        digest: config_digest.to_string(),
        size: total_size,
        created,
        architecture,
        os,
        layer_count: layers.len(),
        layers,
    }))
}

/// List all cached images.
pub fn list_images() -> Result<Vec<ImageInfo>> {
    let root = Path::new(STORAGE_ROOT);
    let manifests_dir = root.join(MANIFESTS_DIR);

    if !manifests_dir.exists() {
        return Ok(Vec::new());
    }

    let mut images = Vec::new();

    for entry in std::fs::read_dir(&manifests_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "json").unwrap_or(false) {
            // Extract image name from filename
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(unsanitize_image_name)
                .unwrap_or_default();

            if let Ok(Some(info)) = query_image(&name) {
                images.push(info);
            }
        }
    }

    Ok(images)
}

/// Run garbage collection.
pub fn garbage_collect(dry_run: bool) -> Result<u64> {
    let root = Path::new(STORAGE_ROOT);
    let layers_dir = root.join(LAYERS_DIR);
    let manifests_dir = root.join(MANIFESTS_DIR);

    // Collect all referenced layers
    let mut referenced_layers = std::collections::HashSet::new();

    if manifests_dir.exists() {
        for entry in std::fs::read_dir(&manifests_dir)? {
            let entry = entry?;
            let content = std::fs::read_to_string(entry.path())?;
            if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(layers) = manifest["layers"].as_array() {
                    for layer in layers {
                        if let Some(digest) = layer["digest"].as_str() {
                            let id = digest.strip_prefix("sha256:").unwrap_or(digest);
                            referenced_layers.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }

    // Find unreferenced layers
    let mut freed = 0u64;

    if layers_dir.exists() {
        for entry in std::fs::read_dir(&layers_dir)? {
            let entry = entry?;
            let layer_id = entry.file_name().to_string_lossy().to_string();

            if !referenced_layers.contains(&layer_id) {
                let size = dir_size(&entry.path()).unwrap_or(0);
                info!(layer = %layer_id, size = size, dry_run = dry_run, "unreferenced layer");

                if !dry_run {
                    std::fs::remove_dir_all(entry.path())?;
                }

                freed += size;
            }
        }
    }

    Ok(freed)
}

/// Prepare an overlay filesystem for a workload.
pub fn prepare_overlay(image: &str, workload_id: &str) -> Result<OverlayInfo> {
    let root = Path::new(STORAGE_ROOT);

    // Ensure image exists
    let info =
        query_image(image)?.ok_or_else(|| StorageError(format!("image not found: {}", image)))?;

    // Create overlay directories
    let overlay_root = root.join(OVERLAYS_DIR).join(workload_id);
    let upper_path = overlay_root.join("upper");
    let work_path = overlay_root.join("work");
    let merged_path = overlay_root.join("merged");

    std::fs::create_dir_all(&upper_path)?;
    std::fs::create_dir_all(&work_path)?;
    std::fs::create_dir_all(&merged_path)?;

    // Set up DNS resolution BEFORE mounting (TSI intercepts writes to mounted overlays)
    let upper_etc = upper_path.join("etc");
    std::fs::create_dir_all(&upper_etc)?;
    let resolv_path = upper_etc.join("resolv.conf");
    if let Err(e) = std::fs::write(&resolv_path, "nameserver 8.8.8.8\nnameserver 1.1.1.1\n") {
        warn!(error = %e, "failed to write resolv.conf to upper layer");
    }

    // Create /dev directory in upper layer - we'll bind mount the real /dev later
    let upper_dev = upper_path.join("dev");
    std::fs::create_dir_all(&upper_dev)?;

    // Build lowerdir from layers (in order)
    let lowerdirs: Vec<String> = info
        .layers
        .iter()
        .rev() // Reverse for overlay order (top layer first)
        .map(|digest| {
            let id = digest.strip_prefix("sha256:").unwrap_or(digest);
            root.join(LAYERS_DIR).join(id).display().to_string()
        })
        .collect();

    let lowerdir = lowerdirs.join(":");

    // Verify layer paths exist before mounting
    for layer_path in &lowerdirs {
        let path = Path::new(layer_path);
        if !path.exists() {
            return Err(StorageError(format!("layer path does not exist: {}", layer_path)));
        }
        // Check if layer has contents
        if let Ok(mut entries) = std::fs::read_dir(path) {
            if entries.next().is_none() {
                warn!(layer = %layer_path, "layer directory is empty");
            }
        }
    }

    // Mount overlay
    let mount_opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        lowerdir,
        upper_path.display(),
        work_path.display()
    );

    debug!(mount_opts = %mount_opts, merged_path = %merged_path.display(), "mounting overlay");

    let output = Command::new("mount")
        .args(["-t", "overlay", "overlay", "-o", &mount_opts])
        .arg(&merged_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(StorageError(format!("failed to mount overlay: {}", stderr)));
    }

    // Verify mount succeeded by checking if merged directory has contents
    let merged_entry_count = std::fs::read_dir(&merged_path)
        .map(|entries| entries.count())
        .unwrap_or(0);

    if merged_entry_count == 0 {
        warn!(
            workload_id = %workload_id,
            merged_path = %merged_path.display(),
            "overlay mount returned success but merged directory is empty"
        );
        // Try to get more info about the mount state
        if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
            let merged_str = merged_path.to_string_lossy();
            let is_mounted = mounts.lines().any(|line| line.contains(&*merged_str));
            warn!(is_mounted = is_mounted, "mount point status");
        }
    }

    info!(workload_id = %workload_id, entry_count = merged_entry_count, "overlay mounted");

    // Create OCI bundle directory structure
    // crun will use this bundle to run containers
    let bundle_path = overlay_root.join("bundle");
    std::fs::create_dir_all(&bundle_path)?;

    // Create symlink: bundle/rootfs -> ../merged
    let rootfs_link = bundle_path.join("rootfs");
    if !rootfs_link.exists() {
        std::os::unix::fs::symlink("../merged", &rootfs_link)
            .map_err(|e| StorageError(format!("failed to create rootfs symlink: {}", e)))?;
    }

    debug!(bundle = %bundle_path.display(), "OCI bundle directory created");

    Ok(OverlayInfo {
        rootfs_path: merged_path.display().to_string(),
        upper_path: upper_path.display().to_string(),
        work_path: work_path.display().to_string(),
    })
}

/// Clean up an overlay filesystem.
pub fn cleanup_overlay(workload_id: &str) -> Result<()> {
    let root = Path::new(STORAGE_ROOT);
    let overlay_root = root.join(OVERLAYS_DIR).join(workload_id);
    let merged_path = overlay_root.join("merged");

    // Unmount if mounted
    if merged_path.exists() {
        let _ = Command::new("umount").arg(&merged_path).status();
    }

    // Remove overlay directories
    if overlay_root.exists() {
        std::fs::remove_dir_all(&overlay_root)?;
    }

    info!(workload_id = %workload_id, "overlay cleaned up");
    Ok(())
}

/// Result of running a command.
pub struct RunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run a command in an image's overlay rootfs using crun OCI runtime.
/// Uses a persistent overlay per image for fast repeated execution.
pub fn run_command(
    image: &str,
    command: &[String],
    env: &[(String, String)],
    workdir: Option<&str>,
    mounts: &[(String, String, bool)],
    timeout_ms: Option<u64>,
) -> Result<RunResult> {
    // Validate inputs
    crate::oci::validate_image_reference(image).map_err(StorageError::new)?;
    crate::oci::validate_env_vars(env).map_err(StorageError::new)?;

    // Use consistent workload ID per image for overlay reuse
    let workload_id = format!("persistent-{}", sanitize_image_name(image));

    // Check if overlay is already mounted
    let overlay = get_or_create_overlay(image, &workload_id)?;
    debug!(rootfs = %overlay.rootfs_path, "using overlay for command execution");

    // Setup volume mounts (mount virtiofs to staging area)
    let mounted_paths = setup_volume_mounts(&overlay.rootfs_path, mounts)?;

    // Get bundle path
    let overlay_root = Path::new(STORAGE_ROOT)
        .join(OVERLAYS_DIR)
        .join(&workload_id);
    let bundle_path = overlay_root.join("bundle");

    // Create OCI spec
    let workdir_str = workdir.unwrap_or("/");
    let mut spec = OciSpec::new(command, env, workdir_str, false);

    // Add virtiofs bind mounts to OCI spec
    for (tag, container_path, read_only) in mounts {
        let virtiofs_mount = Path::new(VIRTIOFS_MOUNT_ROOT).join(tag);
        spec.add_bind_mount(
            &virtiofs_mount.to_string_lossy(),
            container_path,
            *read_only,
        );
    }

    // Write config.json to bundle
    spec.write_to(&bundle_path)
        .map_err(|e| StorageError(format!("failed to write OCI spec: {}", e)))?;

    // Generate unique container ID for this execution
    let container_id = generate_container_id();

    // Run with crun
    let result = run_with_crun(&bundle_path, &container_id, timeout_ms);

    // Note: virtiofs mounts are left in place for reuse
    // They will be cleaned up when the overlay is cleaned up or the VM shuts down
    let _ = mounted_paths; // Suppress unused warning

    result
}

/// Prepare for running a command - returns the rootfs path.
/// This is used by interactive mode which spawns the command separately.
pub fn prepare_for_run(image: &str) -> Result<String> {
    // Use consistent workload ID per image for overlay reuse
    let workload_id = format!("persistent-{}", sanitize_image_name(image));

    // Check if overlay is already mounted
    let overlay = get_or_create_overlay(image, &workload_id)?;
    debug!(rootfs = %overlay.rootfs_path, "prepared overlay for interactive run");

    Ok(overlay.rootfs_path)
}

/// Setup volume mounts for a rootfs (public wrapper).
pub fn setup_mounts(rootfs: &str, mounts: &[(String, String, bool)]) -> Result<()> {
    let _mounted_paths = setup_volume_mounts(rootfs, mounts)?;
    Ok(())
}

/// Directory where virtiofs mounts are mounted in the guest.
const VIRTIOFS_MOUNT_ROOT: &str = "/mnt/virtiofs";

/// Setup volume mounts by mounting virtiofs and bind-mounting into the rootfs.
fn setup_volume_mounts(rootfs: &str, mounts: &[(String, String, bool)]) -> Result<Vec<PathBuf>> {
    let mut mounted_paths = Vec::new();

    for (tag, container_path, read_only) in mounts {
        debug!(tag = %tag, container_path = %container_path, read_only = %read_only, "setting up volume mount");

        // First, mount the virtiofs device at a staging location
        let virtiofs_mount = Path::new(VIRTIOFS_MOUNT_ROOT).join(tag);
        std::fs::create_dir_all(&virtiofs_mount)?;

        // Check if already mounted
        if !is_mountpoint(&virtiofs_mount) {
            info!(tag = %tag, mount_point = %virtiofs_mount.display(), "mounting virtiofs");

            let status = Command::new("mount")
                .args(["-t", "virtiofs", tag])
                .arg(&virtiofs_mount)
                .status()?;

            if !status.success() {
                warn!(tag = %tag, "failed to mount virtiofs device");
                continue;
            }
        }

        // Now bind-mount into the container rootfs
        let target_path = format!("{}{}", rootfs, container_path);
        std::fs::create_dir_all(&target_path)?;

        // Check if already bind-mounted
        if !is_mountpoint(Path::new(&target_path)) {
            info!(
                source = %virtiofs_mount.display(),
                target = %target_path,
                read_only = %read_only,
                "bind-mounting into container"
            );

            let args = ["--bind", &virtiofs_mount.to_string_lossy(), &target_path];

            let status = Command::new("mount").args(args).status()?;

            if !status.success() {
                warn!(target = %target_path, "failed to bind-mount");
                continue;
            }

            // Remount read-only if requested
            if *read_only {
                let _ = Command::new("mount")
                    .args(["-o", "remount,ro,bind", &target_path])
                    .status();
            }
        }

        mounted_paths.push(PathBuf::from(target_path));
    }

    Ok(mounted_paths)
}

/// Get existing overlay or create new one.
fn get_or_create_overlay(image: &str, workload_id: &str) -> Result<OverlayInfo> {
    let root = Path::new(STORAGE_ROOT);
    let overlay_root = root.join(OVERLAYS_DIR).join(workload_id);
    let merged_path = overlay_root.join("merged");

    // Check if already mounted
    if merged_path.exists() && is_mountpoint(&merged_path) {
        debug!(workload_id = %workload_id, "reusing existing overlay");
        return Ok(OverlayInfo {
            rootfs_path: merged_path.display().to_string(),
            upper_path: overlay_root.join("upper").display().to_string(),
            work_path: overlay_root.join("work").display().to_string(),
        });
    }

    // Create new overlay
    prepare_overlay(image, workload_id)
}

/// Check if a path is a mountpoint.
/// Check if a path is a mountpoint (delegates to paths::is_mount_point).
fn is_mountpoint(path: &Path) -> bool {
    paths::is_mount_point(path)
}

/// Run a command using crun OCI runtime (one-shot execution).
///
/// This uses `crun run` which creates, starts, waits, and deletes the container
/// in a single operation. Stdout and stderr are captured.
fn run_with_crun(
    bundle_dir: &Path,
    container_id: &str,
    timeout_ms: Option<u64>,
) -> Result<RunResult> {
    info!(
        container_id = %container_id,
        bundle = %bundle_dir.display(),
        timeout_ms = ?timeout_ms,
        "running container with crun"
    );

    // Spawn the container using CrunCommand
    let mut child = CrunCommand::run(bundle_dir, container_id)
        .capture_output()
        .spawn()
        .map_err(|e| {
            StorageError(format!(
                "failed to spawn crun: {}. Is crun installed at {}?",
                e, paths::CRUN_PATH
            ))
        })?;

    // Capture container_id for the cleanup closure
    let cid = container_id.to_string();

    // Wait with timeout, cleaning up container on timeout
    let result = wait_with_timeout_and_cleanup(&mut child, timeout_ms, || {
        // Kill and delete the container on timeout
        let _ = CrunCommand::kill(&cid, "SIGKILL").status();
        let _ = CrunCommand::delete(&cid, true).status();
    })?;

    // Convert WaitResult to RunResult
    match result {
        WaitResult::Completed { exit_code, output } => {
            info!(
                container_id = %container_id,
                exit_code = exit_code,
                stdout_len = output.stdout.len(),
                stderr_len = output.stderr.len(),
                "container finished"
            );
            Ok(RunResult {
                exit_code,
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
        WaitResult::TimedOut { output, timeout_ms } => {
            warn!(
                container_id = %container_id,
                timeout_ms = timeout_ms,
                "container timed out"
            );
            Ok(RunResult {
                exit_code: TIMEOUT_EXIT_CODE,
                stdout: output.stdout,
                stderr: format!(
                    "{}\ncontainer timed out after {}ms",
                    output.stderr, timeout_ms
                ),
            })
        }
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Run a crane command with the given operation.
fn run_crane(operation: &str, image: &str, platform: Option<&str>) -> Result<String> {
    let mut cmd = Command::new("crane");
    cmd.arg(operation).arg(image);

    if let Some(p) = platform {
        cmd.arg("--platform").arg(p);
    }

    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(StorageError(format!("crane {} failed: {}", operation, stderr)));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Run crane manifest command.
fn crane_manifest(image: &str, platform: Option<&str>) -> Result<String> {
    run_crane("manifest", image, platform)
}

/// Run crane config command.
fn crane_config(image: &str, platform: Option<&str>) -> Result<String> {
    run_crane("config", image, platform)
}

/// Sanitize image name for use as filename.
fn sanitize_image_name(image: &str) -> String {
    image.replace(['/', ':', '@'], "_")
}

/// Reverse sanitization.
fn unsanitize_image_name(name: &str) -> String {
    // This is approximate - we lose some info
    name.replacen('_', "/", 1).replacen('_', ":", 1)
}

/// Get disk usage for a path.
#[allow(unused_variables)] // path is used only on Linux
fn get_disk_usage(path: &Path) -> Result<(u64, u64)> {
    // Use statvfs on Linux
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::mem::MaybeUninit;

        let path_cstr = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| StorageError("invalid path".into()))?;

        unsafe {
            let mut stat: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();
            if libc::statvfs(path_cstr.as_ptr(), stat.as_mut_ptr()) != 0 {
                return Err(std::io::Error::last_os_error().into());
            }

            let stat = stat.assume_init();
            let total = stat.f_blocks * stat.f_frsize;
            let free = stat.f_bfree * stat.f_frsize;
            let used = total - free;

            Ok((total as u64, used as u64))
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok((0, 0))
    }
}

/// Count entries in a directory.
fn count_entries(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }

    Ok(std::fs::read_dir(path)?.count())
}

/// Calculate directory size recursively.
fn dir_size(path: &Path) -> Result<u64> {
    let mut size = 0;

    if path.is_file() {
        return Ok(std::fs::metadata(path)?.len());
    }

    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            size += std::fs::metadata(&path)?.len();
        } else if path.is_dir() {
            size += dir_size(&path)?;
        }
    }

    Ok(size)
}
