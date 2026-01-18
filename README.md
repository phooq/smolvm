# smolvm

OCI-native microVM runtime. Run containers in lightweight VMs using [libkrun](https://github.com/containers/libkrun).

> **Alpha** - APIs may change. Not for production.

## Quick Start

### Prerequisites (macOS)

```bash
brew tap slp/krun
brew install libkrun@1.15.1 libkrunfw
```

> **Note:** Version 1.15.1 is required. Later versions may have compatibility issues.

### Build

```bash
# Clone and build
git clone https://github.com/smolvm/smolvm.git
cd smolvm

# Build agent rootfs (downloads Alpine + crane)
./scripts/build-agent-rootfs.sh

# Cross-compile agent binary for Linux (runs inside VM)
# Option 1: Using Docker (recommended)
docker run --rm -v "$(pwd):/work" -w /work rust:alpine sh -c \
  "apk add musl-dev && cargo build --release -p smolvm-agent"

# Option 2: Using cross (if installed)
cross build --release -p smolvm-agent --target aarch64-unknown-linux-musl

# Copy agent binary to rootfs
cp target/release/smolvm-agent \
  ~/Library/Application\ Support/smolvm/agent-rootfs/usr/local/bin/

# Build smolvm CLI
cargo build --release

# Sign binary (required for Hypervisor.framework on macOS)
codesign --entitlements smolvm.entitlements --force -s - ./target/release/smolvm
```

### Rebuilding the Agent

When you modify code in `crates/smolvm-agent/` or `crates/smolvm-protocol/`, you must rebuild AND reinstall the agent:

```bash
# Force clean rebuild (important after protocol changes!)
docker run --rm -v "$(pwd):/work" -w /work rust:alpine sh -c \
  "apk add musl-dev && \
   rm -rf target/release/deps/smolvm_protocol* \
          target/release/deps/smolvm_agent* \
          target/release/.fingerprint/smolvm-protocol* \
          target/release/.fingerprint/smolvm-agent* && \
   cargo build --release -p smolvm-agent"

# Copy to rootfs
cp target/release/smolvm-agent \
  ~/Library/Application\ Support/smolvm/agent-rootfs/usr/local/bin/

# IMPORTANT: Restart the microvm to pick up new binary
./target/release/smolvm microvm stop
```

**Common mistake:** After protocol changes, Docker's cargo cache may not detect the change. Always clean the fingerprint files as shown above.

**Shortcut:** Use the helper script:
```bash
./scripts/rebuild-agent.sh          # Normal rebuild
./scripts/rebuild-agent.sh --clean  # Force clean (after protocol changes)
```

### Test

```bash
# Set library path (adjust for your Homebrew prefix if different)
export DYLD_LIBRARY_PATH=/opt/homebrew/opt/libkrun@1.15.1/lib:/opt/homebrew/lib

# Basic test (ephemeral - stops microvm after)
./target/release/smolvm microvm run alpine:latest echo "Hello World"

# With network
./target/release/smolvm microvm run --net alpine:latest wget -qO- ifconfig.me

# With volume mount
mkdir -p /tmp/test && echo "hello" > /tmp/test/file.txt
./target/release/smolvm microvm run -v /tmp/test:/data alpine:latest cat /data/file.txt

# Persistent microvm (faster for repeated commands)
./target/release/smolvm microvm exec echo "Fast"
./target/release/smolvm microvm exec ls /
./target/release/smolvm microvm stop
```

## Usage

### CLI Structure

```
smolvm
├── microvm           # All microvm operations
│   ├── run           # Ephemeral: starts microvm, runs command, stops microvm
│   ├── exec          # Persistent: executes command, microvm keeps running
│   ├── create        # Create named VM configuration
│   ├── start         # Start a microvm (named or default)
│   ├── stop          # Stop a microvm (named or default)
│   ├── delete        # Delete a named VM configuration
│   ├── status        # Show microvm status
│   └── ls            # List all named VMs
└── container         # Manage containers inside microvm
```

### run vs exec

| Command | Execution Context | MicroVM Lifecycle | Use Case |
|---------|-------------------|-------------------|----------|
| `microvm run` | Inside container (OCI image) | Starts → runs → **stops** | One-off container commands |
| `microvm exec` | Directly in VM (Alpine rootfs) | Starts → runs → **keeps running** | VM-level operations, debugging |

### Run (Ephemeral Container)

```bash
smolvm microvm run [OPTIONS] <IMAGE> [COMMAND]

smolvm microvm run alpine:latest echo "Hello"              # Stops microvm after
smolvm microvm run -e FOO=bar alpine:latest env            # Environment vars
smolvm microvm run -v /host/path:/guest/path alpine:latest # Volume mount
smolvm microvm run --timeout 30s alpine:latest sleep 60    # Timeout (exit 124)
smolvm microvm run -p 8080:80 nginx:latest                 # Port forwarding
smolvm microvm run -it alpine:latest /bin/sh              # Interactive shell
```

### Exec (Direct VM Access)

Executes commands directly in the VM's Alpine rootfs (not in a container):

```bash
smolvm microvm exec [OPTIONS] <COMMAND>

smolvm microvm exec echo "First"              # Starts microvm (~2s)
smolvm microvm exec echo "Second"             # Reuses microvm (~50ms)
smolvm microvm exec cat /etc/os-release       # Shows Alpine (VM's OS)
smolvm microvm exec ls /storage               # Access VM storage
smolvm microvm exec -it /bin/sh               # Interactive shell in VM

# Manage microvm
smolvm microvm status
smolvm microvm stop
```

### Named VMs (Isolated MicroVMs)

Each named VM gets its own isolated microvm, storage, and configuration:

```bash
# Create VM configurations
smolvm microvm create --name web --cpus 2 --mem 512 node:20 npm start
smolvm microvm create --name db --cpus 2 --mem 1024 postgres:16

# Run them simultaneously (each in separate terminal)
smolvm microvm start web   # Runs in its own microvm
smolvm microvm start db    # Runs in its own microvm (parallel!)

# Exec directly into a running named microvm (VM-level, not container)
smolvm microvm exec --name web ls /storage

# Check status of specific microvm
smolvm microvm status web
smolvm microvm status db

# Stop specific microvm
smolvm microvm stop web

# List saved VMs
smolvm microvm ls
smolvm microvm ls -v  # verbose

# Remove saved configuration
smolvm microvm delete myvm
smolvm microvm delete myvm -f  # skip confirmation
```

**Isolation:**
- Each named VM has its own microvm process
- Separate storage disk per VM (`~/.cache/smolvm/vms/{name}/`)
- Can run multiple VMs simultaneously

## Options

### microvm run Options

| Flag | Description |
|------|-------------|
| `--cpus N` | Number of vCPUs (default: 1) |
| `--mem N` | Memory in MiB (default: 512) |
| `-e KEY=VAL` | Environment variable |
| `-v host:guest[:ro]` | Volume mount (directories only) |
| `-w /path` | Working directory |
| `--net` | Enable network egress |
| `-p HOST:GUEST` | Port forwarding (e.g., `-p 8080:80`) |
| `--timeout DURATION` | Kill command after duration (e.g., `30s`, `5m`) |
| `-i` | Keep stdin open (interactive mode) |
| `-t` | Allocate pseudo-TTY |

### microvm create Options

| Flag | Description |
|------|-------------|
| `--name NAME` | VM name (required) |
| `--cpus N` | Number of vCPUs (default: 1) |
| `--mem N` | Memory in MiB (default: 256) |
| `-e KEY=VAL` | Environment variable |
| `-v host:guest[:ro]` | Volume mount (directories only) |
| `-w /path` | Working directory |

### microvm exec Options

Executes commands directly in the VM (not in a container):

| Flag | Description |
|------|-------------|
| `--name NAME` | Named microvm to exec into |
| `-w /path` | Working directory in VM |
| `-e KEY=VAL` | Environment variable |
| `--timeout DURATION` | Kill command after duration (e.g., `30s`, `5m`) |
| `-i` | Keep stdin open (interactive mode) |
| `-t` | Allocate pseudo-TTY |

## Troubleshooting

```bash
# Enable debug logging
RUST_LOG=debug ./target/release/smolvm microvm run alpine:latest

# Check agent logs
cat ~/Library/Caches/smolvm/agent-console.log

# Kill stuck microvm
smolvm microvm stop
pkill -9 -f krun
```

## Limitations

- **Network access**: libkrun's TSI (Transparent Socket Impersonation) provides networking. Basic socket operations work well. Some tools like busybox `wget` may show warnings about connection closing but still receive data successfully. The agent uses Go-based `crane` for reliable image pulling.
- Volume mounts must be directories (virtiofs limitation)
- No x86 emulation on ARM Macs (host arch = guest arch)
- PTY mode (`-t`) streams output but doesn't handle all terminal features yet

## License

Apache-2.0
