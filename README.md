# smolvm, an OCI-native microVM runtime with batteries included. 

You can use this to... 
- run microVM's locally on both macOS and Linux with minimal setup
- run and sandbox coding agents locally
- run containers within microvm for improved isolation

Compared to existing tools like Firecracker, kata containers, etc.. smolvm differentiates by being easy to setup and runs on dev machines locally.

> **Alpha** - APIs can change, there may be bugs. Please submit a report or contribute!

## Install

```bash
# WIP-  macOS (Homebrew)
brew install smolvm/tap/smolvm

# From source
./scripts/build-dist.sh && ./scripts/install-local.sh
```

## Usage

```bash
# Quick sandbox (ephemeral)
smolvm sandbox run alpine:latest echo "Hello"
smolvm sandbox run -v ~/code:/code python:3.12 python /code/script.py

# MicroVM management
smolvm microvm run alpine:latest echo "Hello"      # Run and stop
smolvm microvm exec echo "Fast"                     # Persistent (~50ms warm)
smolvm microvm exec -it /bin/sh                     # Interactive shell
smolvm microvm stop

# Named VMs
smolvm microvm create --name web --cpus 2 --mem 512 node:20
smolvm microvm start web
smolvm microvm stop web
smolvm microvm ls
smolvm microvm delete web

# Containers inside VMs
smolvm container create myvm alpine -- sleep 300
smolvm container start myvm <id>
smolvm container exec myvm <id> -- ps aux
smolvm container stop myvm <id>
smolvm container ls myvm

# Server mode (HTTP API)
smolvm serve                          # localhost:8080
smolvm serve --listen 0.0.0.0:9000    # Custom address
```

## Options

| Flag | Description |
|------|-------------|
| `-e KEY=VAL` | Environment variable |
| `-v host:guest[:ro]` | Volume mount (directories only) |
| `-w /path` | Working directory |
| `-p HOST:GUEST` | Port forwarding |
| `--cpus N` | vCPU count |
| `--mem N` | Memory (MiB) |
| `--net` | Enable network |
| `--timeout 30s` | Execution timeout |
| `-i` | Interactive (stdin) |
| `-t` | Allocate TTY |

## Platform Support

| Host | Guest | Status |
|------|-------|--------|
| macOS Apple Silicon | arm64 Linux | ✅ |
| macOS Apple Silicon | x86_64 Linux | WIP (Rosetta 2, [experimental]) |
| macOS Intel | x86_64 Linux | ? | No machine to test this.
| Linux x86_64 | x86_64 Linux | ~ | WIP

## Known Limitations

- **Container rootfs writes**: Writes to container filesystem (`/tmp`, `/home`, etc.) fail with "Connection reset by network" due to a libkrun TSI bug with overlayfs. **Writes to mounted volumes work** - see below.
- **Volume mounts**: Directories only (no single files)
- **Rosetta 2**: Required for x86_64 images on Apple Silicon (`softwareupdate --install-rosetta`)
- **macOS**: Binary must be signed with Hypervisor.framework entitlements

### Coding Agent File Writes

```bash
# Works: write to mounted volume (virtiofs bypasses overlayfs)
smolvm sandbox run -v ~/code:/workspace python:3.12 -- python -c "open('/workspace/out.py', 'w').write('hello')"

# Fails: write to container rootfs (overlayfs triggers TSI bug)
smolvm sandbox run python:3.12 -- python -c "open('/tmp/out.py', 'w').write('hello')"
```

Mount your workspace and ensure the agent writes only there.

## Storage

OCI images and container overlays are stored in a sparse ext4 disk image:

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/smolvm/storage.raw` |
| Linux | `~/.local/share/smolvm/storage.raw` |

Default size is 20 GB (sparse - only uses actual written space). The ext4 filesystem inside the VM handles Linux case-sensitivity correctly, avoiding issues with macOS's case-insensitive filesystem.

## AI Usage disclosure

AI was used to write code in this project.

I write code until the first working version. 

AI then review and refactor, also acts as a partner to discuss design trade-offs.

## Contributions

AI defaults to copying existing projects upon new obstacles and was not suitable for this project, given lack of existing projects to derive off of.

Please ensure to have human oversight for opening a PR.

## License

Apache-2.0
