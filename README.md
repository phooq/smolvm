# smolvm

OCI-native microVM runtime. Run containers in lightweight VMs using [libkrun](https://github.com/containers/libkrun).

> **Alpha** - APIs may change. Not for production.

## Quick Start

### Prerequisites (macOS)

```bash
brew install libkrun@1.15.1 libkrunfw
```

### Build

```bash
# Clone and build
git clone https://github.com/smolvm/smolvm.git
cd smolvm

# Build agent (cross-compile for Linux guest)
./scripts/build-agent-rootfs.sh

# Build smolvm
cargo build --release

# Sign binary (required for Hypervisor.framework)
codesign --entitlements smolvm.entitlements --force -s - ./target/release/smolvm
```

### Test

```bash
# Set library path
export DYLD_LIBRARY_PATH=$PWD/lib

# Basic test
./target/release/smolvm run alpine:latest echo "Hello World"

# With network
./target/release/smolvm run --net alpine:latest wget -qO- ifconfig.me

# With volume mount
mkdir -p /tmp/test && echo "hello" > /tmp/test/file.txt
./target/release/smolvm run -v /tmp/test:/data alpine:latest cat /data/file.txt

# Agent mode (faster for repeated commands)
./target/release/smolvm exec alpine:latest echo "Fast"
./target/release/smolvm exec alpine:latest ls /
./target/release/smolvm agent stop
```

## Usage

### run vs exec

| Command | Agent Lifecycle | Use Case |
|---------|-----------------|----------|
| `run` | Starts → runs → **stops** | One-off commands |
| `exec` | Starts → runs → **keeps running** | Repeated commands |

### Run (Ephemeral)

```bash
smolvm run [OPTIONS] <IMAGE> [COMMAND]

smolvm run alpine:latest echo "Hello"              # Stops agent after
smolvm run -e FOO=bar alpine:latest env            # Environment vars
smolvm run -v /host/path:/guest/path alpine:latest # Volume mount
smolvm run --timeout 30s alpine:latest sleep 60    # Timeout (exit 124)
smolvm run -p 8080:80 nginx:latest                 # Port forwarding
smolvm run -it alpine:latest /bin/sh              # Interactive shell
```

### Exec (Persistent Agent)

```bash
smolvm exec [OPTIONS] <IMAGE> [COMMAND]

smolvm exec alpine:latest echo "First"   # Starts agent (~2s)
smolvm exec alpine:latest echo "Second"  # Reuses agent (~50ms)
smolvm exec -v ~/project:/workspace node:latest npm test
smolvm exec -it alpine:latest /bin/sh   # Interactive shell (agent persists)
smolvm exec -p 3000:3000 node:latest npm start  # Port forward

# Manage agent
smolvm agent status
smolvm agent stop
```

### Named VMs (Isolated Agents)

Each named VM gets its own isolated agent, storage, and configuration:

```bash
# Create VM configurations
smolvm create --name web --cpus 2 --mem 512 node:20 npm start
smolvm create --name db --cpus 2 --mem 1024 postgres:16

# Run them simultaneously (each in separate terminal)
smolvm start web   # Runs in its own agent VM
smolvm start db    # Runs in its own agent VM (parallel!)

# Exec into a running named VM
smolvm exec --name web node:20 npm test

# Check status of specific VM's agent
smolvm agent status --name web
smolvm agent status --name db

# Stop specific VM's agent
smolvm agent stop --name web

# List saved VMs
smolvm list
smolvm list -v  # verbose

# Remove saved configuration
smolvm delete myvm
smolvm delete myvm -f  # skip confirmation
```

**Isolation:**
- Each named VM has its own agent process
- Separate storage disk per VM (`~/.cache/smolvm/vms/{name}/`)
- Can run multiple VMs simultaneously

## Options

| Flag | Description |
|------|-------------|
| `--cpus N` | Number of vCPUs (default: 1) |
| `--mem N` | Memory in MiB (default: 256) |
| `-e KEY=VAL` | Environment variable |
| `-v host:guest[:ro]` | Volume mount (directories only) |
| `-w /path` | Working directory |
| `-p HOST:GUEST` | Port forwarding (e.g., `-p 8080:80`) |
| `--timeout DURATION` | Kill command after duration (e.g., `30s`, `5m`) |
| `-i` | Keep stdin open (interactive mode) |
| `-t` | Allocate pseudo-TTY |

## Troubleshooting

```bash
# Enable debug logging
RUST_LOG=debug ./target/release/smolvm run alpine:latest

# Check agent logs
cat ~/Library/Caches/smolvm/agent-console.log

# Kill stuck agent
smolvm agent stop
pkill -9 -f krun
```

## Limitations

- Volume mounts must be directories (virtiofs limitation)
- No x86 emulation on ARM Macs (host arch = guest arch)
- PTY mode (`-t`) streams output but doesn't handle all terminal features yet

## License

MIT
