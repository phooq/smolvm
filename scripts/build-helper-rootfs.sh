#!/bin/bash
# Build the helper VM rootfs
#
# This script creates an Alpine-based rootfs with:
# - crane (for OCI image operations)
# - smolvm-helper daemon
# - Required utilities (jq, e2fsprogs, etc.)
#
# Usage: ./scripts/build-helper-rootfs.sh [output-dir]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="${1:-$PROJECT_ROOT/target/helper-rootfs}"

# Alpine version
ALPINE_VERSION="3.19"
ALPINE_ARCH="aarch64"  # Change to x86_64 for Intel

# Detect architecture
case "$(uname -m)" in
    arm64|aarch64)
        ALPINE_ARCH="aarch64"
        CRANE_ARCH="arm64"
        ;;
    x86_64|amd64)
        ALPINE_ARCH="x86_64"
        CRANE_ARCH="x86_64"
        ;;
    *)
        echo "Unsupported architecture: $(uname -m)"
        exit 1
        ;;
esac

ALPINE_MIRROR="https://dl-cdn.alpinelinux.org/alpine"
ALPINE_MINIROOTFS="alpine-minirootfs-${ALPINE_VERSION}.0-${ALPINE_ARCH}.tar.gz"
ALPINE_URL="${ALPINE_MIRROR}/v${ALPINE_VERSION}/releases/${ALPINE_ARCH}/${ALPINE_MINIROOTFS}"

# Crane version
CRANE_VERSION="0.19.0"
CRANE_URL="https://github.com/google/go-containerregistry/releases/download/v${CRANE_VERSION}/go-containerregistry_Linux_${CRANE_ARCH}.tar.gz"

echo "Building helper rootfs..."
echo "  Alpine: ${ALPINE_VERSION} (${ALPINE_ARCH})"
echo "  Crane: ${CRANE_VERSION}"
echo "  Output: ${OUTPUT_DIR}"

# Create output directory
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

# Download Alpine minirootfs
echo "Downloading Alpine minirootfs..."
ALPINE_TAR="/tmp/${ALPINE_MINIROOTFS}"
if [ ! -f "$ALPINE_TAR" ]; then
    curl -fsSL -o "$ALPINE_TAR" "$ALPINE_URL"
fi

# Extract Alpine
echo "Extracting Alpine..."
tar -xzf "$ALPINE_TAR" -C "$OUTPUT_DIR"

# Download crane
echo "Downloading crane..."
CRANE_TAR="/tmp/crane-${CRANE_VERSION}.tar.gz"
if [ ! -f "$CRANE_TAR" ]; then
    curl -fsSL -o "$CRANE_TAR" "$CRANE_URL"
fi

# Extract crane to rootfs
echo "Installing crane..."
mkdir -p "$OUTPUT_DIR/usr/local/bin"
tar -xzf "$CRANE_TAR" -C "$OUTPUT_DIR/usr/local/bin" crane

# Install additional packages using chroot (requires root)
echo "Note: To install additional packages, run with sudo or in a container"

# Create necessary directories
mkdir -p "$OUTPUT_DIR/storage"
mkdir -p "$OUTPUT_DIR/etc/init.d"
mkdir -p "$OUTPUT_DIR/run"

# Remove existing init (it's a symlink to busybox)
rm -f "$OUTPUT_DIR/sbin/init"

# Create init script
cat > "$OUTPUT_DIR/sbin/init" << 'INIT_EOF'
#!/bin/sh
# Helper VM init script

# Mount essential filesystems
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev

# Create device nodes if needed
[ -e /dev/vda ] || mknod /dev/vda b 253 0

# Mount storage disk
if [ -b /dev/vda ]; then
    # Check if formatted
    if ! blkid /dev/vda > /dev/null 2>&1; then
        echo "Storage disk not formatted, formatting..."
        mkfs.ext4 -F /dev/vda
    fi

    mount /dev/vda /storage

    # Create directory structure
    mkdir -p /storage/layers
    mkdir -p /storage/configs
    mkdir -p /storage/manifests
    mkdir -p /storage/overlays

    # Mark as formatted
    touch /storage/.smolvm_formatted
fi

# Set up networking (if available)
ip link set lo up 2>/dev/null || true

# Start helper daemon
echo "Starting smolvm-helper..."
exec /usr/local/bin/smolvm-helper
INIT_EOF
chmod +x "$OUTPUT_DIR/sbin/init"

# Create resolv.conf
echo "nameserver 1.1.1.1" > "$OUTPUT_DIR/etc/resolv.conf"

# Create helper daemon placeholder
cat > "$OUTPUT_DIR/usr/local/bin/smolvm-helper" << 'HELPER_EOF'
#!/bin/sh
# Placeholder for smolvm-helper daemon
# The actual binary will be cross-compiled and copied here

echo "smolvm-helper placeholder"
echo "Replace with actual binary built for Linux"

# For now, just run a shell
exec /bin/sh
HELPER_EOF
chmod +x "$OUTPUT_DIR/usr/local/bin/smolvm-helper"

echo ""
echo "Helper rootfs created at: $OUTPUT_DIR"
echo ""
echo "To complete the build:"
echo "1. Cross-compile smolvm-helper for Linux:"
echo "   cross build --release -p smolvm-helper --target aarch64-unknown-linux-musl"
echo ""
echo "2. Copy the binary:"
echo "   cp target/aarch64-unknown-linux-musl/release/smolvm-helper $OUTPUT_DIR/usr/local/bin/"
echo ""
echo "3. (Optional) Install additional packages in a container:"
echo "   docker run --rm -v $OUTPUT_DIR:/rootfs alpine:$ALPINE_VERSION sh -c '"
echo "     apk add --root /rootfs --initdb jq e2fsprogs'"
echo ""
echo "Rootfs size: $(du -sh "$OUTPUT_DIR" | cut -f1)"
