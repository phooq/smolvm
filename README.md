# smolvm

> **Alpha Software** - This project is in early development. APIs and behavior may change. Not recommended for production use.

OCI-native microVM runtime for macOS and Linux.

smolvm runs containers as lightweight microVMs using [libkrun](https://github.com/containers/libkrun), providing VM-level isolation with container-like ergonomics.

## Features

- **OCI-native**: Pull and run container images directly (via buildah)
- **Lightweight**: Fast boot times using libkrun's minimalist approach
- **Secure**: VM-level isolation using Hypervisor.framework (macOS) or KVM (Linux)
- **Simple CLI**: Docker-like command interface

## Requirements

### macOS

- Apple Silicon (M1/M2/M3) or Intel Mac
- macOS 11.0 or later
- [libkrun](https://github.com/containers/libkrun) installed
- [buildah](https://github.com/containers/buildah) installed (for OCI image support)
- Case-sensitive volume at `/Volumes/krunvm` (for buildah storage)

### Linux

- KVM-capable system
- libkrun installed
- buildah installed (for OCI image support)

## Installation

### Option 1: Download Pre-built Release (Recommended)

```bash
# Download and extract
curl -LO https://github.com/smolvm/smolvm/releases/download/v0.1.0/smolvm-0.1.0-darwin-arm64.tar.gz
tar -xzf smolvm-0.1.0-darwin-arm64.tar.gz
cd smolvm-0.1.0-darwin-arm64

# Test it works
./smolvm --help

# Option A: Add to PATH (in ~/.zshrc or ~/.bashrc)
export PATH="/path/to/smolvm-0.1.0-darwin-arm64:$PATH"

# Option B: Create a symlink
sudo ln -s /path/to/smolvm-0.1.0-darwin-arm64/smolvm /usr/local/bin/smolvm
```

The distribution includes all required libraries - no need to install libkrun separately.

### Option 2: Build from Source

```bash
# Prerequisites: Install libkrun and libkrunfw
brew install libkrun libkrunfw buildah

# Clone and build
git clone https://github.com/smolvm/smolvm.git
cd smolvm
cargo build --release

# Sign the binary (macOS only - required for Hypervisor.framework)
codesign --entitlements smolvm.entitlements --force -s - ./target/release/smolvm

# Copy to PATH
cp ./target/release/smolvm /usr/local/bin/
```

### Option 3: Build Distribution Package

```bash
# Build a self-contained distribution with bundled libraries
mkdir -p lib
cp /opt/homebrew/opt/libkrun/lib/libkrun.dylib lib/
cp /opt/homebrew/opt/libkrunfw/lib/libkrunfw.4.dylib lib/

./scripts/build-dist.sh
# Output: dist/smolvm-<version>-<platform>.tar.gz
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `SMOLVM_STORAGE` | Storage volume path for buildah (default: `/Volumes/krunvm`) |
| `RUST_LOG` | Logging level (e.g., `smolvm=debug`) |

## Quick Start

```bash
# Ephemeral: Run a command and exit (container cleaned up automatically)
smolvm run alpine:latest echo "Hello World"

# Persistent: Create a VM that persists across runs
smolvm create --name myvm alpine:latest /bin/sh
smolvm start myvm           # Start in background (daemon mode)
smolvm list                 # See running VMs
smolvm stop myvm            # Stop the VM
smolvm start --foreground myvm  # Start in foreground (interactive)
smolvm delete myvm          # Remove the VM
```

## Commands

### `smolvm run`

Run a VM from a rootfs path or OCI image (ephemeral mode - VM is cleaned up after exit).

```
Usage: smolvm run [OPTIONS] <SOURCE> [COMMAND]...

Arguments:
  <SOURCE>      Rootfs path or OCI image reference
  [COMMAND]...  Command to execute inside the VM (default: /bin/sh)

Options:
      --name <NAME>       VM name (auto-generated if not provided)
      --memory <MEMORY>   Memory in MiB [default: 512]
      --cpus <CPUS>       Number of vCPUs [default: 1]
  -w, --workdir <WORKDIR> Working directory inside the VM
  -e, --env <ENV>         Environment variable (KEY=VALUE), can be repeated
  -v, --volume <VOLUME>   Volume mount (host:guest[:ro]), can be repeated
      --net               Enable network egress (NAT)
      --dns <DNS>         Custom DNS server (requires --net)
  -h, --help              Print help
```

#### Examples

```bash
# Run Alpine with default shell
smolvm run alpine:latest

# Run with custom command
smolvm run alpine:latest cat /etc/os-release

# Run with more resources
smolvm run --memory 1024 --cpus 2 ubuntu:22.04

# Run with network access
smolvm run --net alpine:latest wget -qO- ifconfig.me

# Run with custom DNS
smolvm run --net --dns 8.8.8.8 alpine:latest nslookup google.com

# Run with environment variables
smolvm run -e FOO=bar -e DEBUG=1 alpine:latest env

# Run with volume mounts
smolvm run -v /tmp/data:/data alpine:latest ls /data

# Run with read-only volume
smolvm run -v /etc/hosts:/etc/hosts:ro alpine:latest cat /etc/hosts

# Run with working directory
smolvm run -w /app alpine:latest pwd

# Run with custom name
smolvm run --name my-vm alpine:latest

# Run from local rootfs directory
smolvm run /path/to/rootfs /bin/sh
```

### `smolvm list`

List all VMs (alias: `smolvm ls`).

```
Usage: smolvm list [OPTIONS]

Options:
  -v, --verbose  Show detailed output
      --json     Output as JSON
  -h, --help     Print help
```

#### Examples

```bash
# List VMs in table format
smolvm list
# NAME        STATE    CPUS  MEMORY   PID    ROOTFS
# --------------------------------------------------------------------------------
# web         running  2     1024 MiB 12345  buildah:nginx-...
# dev         stopped  1     512 MiB  -      buildah:alpine-...
# worker      created  4     2048 MiB -      buildah:ubuntu-...

# List with details
smolvm list -v

# Output as JSON
smolvm list --json

# Using alias
smolvm ls
```

### `smolvm create`

Create a VM without starting it. The VM persists until explicitly deleted.

```
Usage: smolvm create [OPTIONS] --name <NAME> <SOURCE> [COMMAND]...

Arguments:
  <SOURCE>      Rootfs path or OCI image reference
  [COMMAND]...  Command to execute when the VM starts

Options:
      --name <NAME>        VM name (required)
      --memory <MEMORY>    Memory in MiB [default: 512]
      --cpus <CPUS>        Number of vCPUs [default: 1]
  -w, --workdir <WORKDIR>  Working directory inside the VM
  -e, --env <ENV>          Environment variable (KEY=VALUE)
  -v, --volume <VOLUME>    Volume mount (host:guest[:ro])
      --net                Enable network egress
      --dns <DNS>          Custom DNS server (requires --net)
  -h, --help               Print help
```

#### Examples

```bash
# Create a VM with a shell
smolvm create --name dev alpine:latest /bin/sh

# Create with custom resources
smolvm create --name worker --memory 1024 --cpus 2 ubuntu:22.04 /bin/bash

# Create with environment and volumes
smolvm create --name app -e DEBUG=1 -v /tmp/data:/data alpine:latest /bin/sh

# Create a long-running service
smolvm create --name web --net alpine:latest /usr/sbin/nginx -g "daemon off;"
```

### `smolvm start`

Start a created or stopped VM.

```
Usage: smolvm start [OPTIONS] <NAME>

Arguments:
  <NAME>  VM name to start

Options:
      --foreground  Run in foreground (don't daemonize)
  -h, --help        Print help
```

#### Examples

```bash
# Start in background (daemon mode)
smolvm start myvm
# Output: Started VM: myvm (PID: 12345)
#         Logs: /tmp/smolvm/myvm.log

# Start in foreground (interactive)
smolvm start --foreground myvm

# Restart a stopped VM
smolvm start myvm
```

### `smolvm stop`

Stop a running VM.

```
Usage: smolvm stop [OPTIONS] <NAME>

Arguments:
  <NAME>  VM name to stop

Options:
  -f, --force          Force stop (SIGKILL after timeout)
      --timeout <SEC>  Timeout before force kill [default: 10]
  -h, --help           Print help
```

#### Examples

```bash
# Graceful stop (SIGTERM)
smolvm stop myvm

# Force stop after 5 seconds
smolvm stop --force --timeout 5 myvm

# Immediate force stop
smolvm stop -f myvm
```

### `smolvm delete`

Delete a VM (alias: `smolvm rm`).

```
Usage: smolvm delete [OPTIONS] <NAME>

Arguments:
  <NAME>  VM name to delete

Options:
  -f, --force  Force deletion without confirmation
  -h, --help   Print help
```

#### Examples

```bash
# Delete with confirmation prompt
smolvm delete my-vm

# Force delete without confirmation
smolvm delete -f my-vm

# Using alias
smolvm rm my-vm
```

## Configuration

smolvm stores configuration in:
- macOS: `~/Library/Preferences/rs.smolvm/smolvm.toml`
- Linux: `~/.config/smolvm/smolvm.toml`

### Configuration Options

```toml
# Default number of CPUs for new VMs
default_cpus = 1

# Default memory in MiB for new VMs
default_mem = 512

# Default DNS server
default_dns = "1.1.1.1"
```

## Architecture

smolvm uses a layered architecture:

```
┌─────────────────────────────────────────┐
│              CLI (clap)                 │
├─────────────────────────────────────────┤
│           VM Configuration              │
├─────────────────────────────────────────┤
│    libkrun Backend (FFI bindings)       │
├─────────────────────────────────────────┤
│  Hypervisor.framework (macOS) / KVM     │
└─────────────────────────────────────────┘
```

### Key Components

| Component | Description |
|-----------|-------------|
| `vm::backend::libkrun` | FFI bindings to libkrun |
| `vm::config` | VM configuration types |
| `rootfs::buildah` | OCI image management via buildah |
| `protocol` | vsock protocol types (for future use) |
| `storage` | Storage disk management |

## Debugging

Enable debug logging:

```bash
RUST_LOG=smolvm=debug smolvm run alpine:latest
```

Enable trace logging (very verbose):

```bash
RUST_LOG=smolvm=trace smolvm run alpine:latest
```

## Manual Testing

Run the smoke tests to verify basic functionality:

```bash
#!/bin/bash
set -e
export DYLD_LIBRARY_PATH=$PWD/lib

echo "=== Smoke Test ==="

echo "1. Basic echo..."
./target/release/smolvm run alpine:latest /bin/echo "OK"

echo "2. Exit code..."
./target/release/smolvm run alpine:latest /bin/sh -c "exit 0" && echo "OK"

echo "3. Environment..."
./target/release/smolvm run -e TEST=passed alpine:latest /bin/sh -c 'echo $TEST' | grep -q passed && echo "OK"

echo "4. Mount (directory)..."
mkdir -p /tmp/smolvm-test
echo "mount-test" > /tmp/smolvm-test/file.txt
./target/release/smolvm run -v /tmp/smolvm-test:/mnt/data:ro alpine:latest cat /mnt/data/file.txt | grep -q mount-test && echo "OK"
rm -rf /tmp/smolvm-test

echo "5. Single-file mount rejection..."
echo "test" > /tmp/single-file.txt
./target/release/smolvm run -v /tmp/single-file.txt:/test.txt alpine:latest echo test 2>&1 | grep -q "single file" && echo "OK"
rm /tmp/single-file.txt

echo "=== All tests passed ==="
```

### Additional Manual Tests

```bash
# Custom resources
smolvm run --memory 1024 --cpus 2 alpine:latest nproc

# Network egress (if supported)
smolvm run --net alpine:latest wget -qO- ifconfig.me

# Named VM
smolvm run --name my-vm alpine:latest echo hello
smolvm list
smolvm delete my-vm

# Different images
smolvm run ubuntu:22.04 cat /etc/os-release
smolvm run debian:bookworm cat /etc/os-release
```

## Comparison with krunvm

smolvm is inspired by [krunvm](https://github.com/containers/krunvm) and shares the same foundation (libkrun + buildah). Here's how they compare:

| Feature | smolvm | krunvm |
|---------|--------|--------|
| Create persistent VMs | `create` | `create` |
| Start VMs | `start` (daemon or foreground) | `start` |
| Stop VMs | `stop` (graceful + force) | - |
| Ephemeral run | `run` | - |
| List VMs | `list` (with state/PID) | `list` |
| Delete VMs | `delete` | `delete` |
| Modify VMs | - | `changevm` |
| Volume mounts | `-v host:guest[:ro]` | `-v/--volume` |
| Port forwarding | - | `-p/--port` |
| Network egress | `--net` | `--net` |
| Custom DNS | `--dns` | - |
| Background mode | Default for `start` | - |
| Process tracking | PID + state in `list` | - |

### Key Differences

- **smolvm has explicit `stop`**: Sends SIGTERM with optional force kill, updates state tracking
- **smolvm has ephemeral `run`**: One-liner execution with automatic cleanup (like `docker run --rm`)
- **smolvm tracks state**: Shows Running/Stopped/Created status and PIDs in `list`
- **smolvm supports daemon mode**: `start` runs in background by default with log files
- **krunvm has `changevm`**: Modify VM configuration after creation
- **krunvm has port forwarding**: Expose guest ports to host (`-p`)

### When to Use Which

- **Use smolvm** if you want Docker-like ephemeral runs, explicit stop commands, or background daemon mode
- **Use krunvm** if you need port forwarding or want to modify VM configs after creation

## Limitations

Current limitations:

- No exec into running VMs (requires guest agent - planned)
- No port forwarding (planned)
- No Rosetta support for x86 images on ARM Macs
- Requires external buildah for OCI image handling
- **Volume mounts must be directories** (virtiofs limitation - see below)
- **Stop uses signals, not graceful shutdown** (see below)

### Volume Mount Limitation

virtiofs only supports mounting directories, not individual files. Attempting to mount a single file will result in an error.

```bash
# This will NOT work:
smolvm run -v /path/to/file.txt:/guest/file.txt alpine:latest

# Instead, mount the parent directory:
smolvm run -v /path/to:/mnt/data alpine:latest cat /mnt/data/file.txt
```

### Stop/Kill Behavior

The `smolvm stop` command sends signals (SIGTERM, then SIGKILL with `--force`) to the VM's host process. This terminates the VM immediately without graceful guest shutdown. For workloads that need clean shutdown:

- Ensure your application handles SIGTERM gracefully
- Use `--timeout` to give the process time to exit cleanly
- Future versions will support vsock-based graceful shutdown via a guest agent

## License

MIT

## Acknowledgments

- [libkrun](https://github.com/containers/libkrun) - Lightweight VM library
- [buildah](https://github.com/containers/buildah) - OCI image builder
- [krunvm](https://github.com/containers/krunvm) - Original inspiration
