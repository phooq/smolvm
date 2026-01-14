#!/bin/bash
# Build a distributable smolvm package
#
# Usage: ./scripts/build-dist.sh
#
# Output: dist/smolvm-<version>-<platform>.tar.gz

set -e

# Configuration
VERSION="${VERSION:-$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)}"
PLATFORM="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
DIST_NAME="smolvm-${VERSION}-${PLATFORM}"
DIST_DIR="dist/${DIST_NAME}"

echo "Building smolvm distribution: ${DIST_NAME}"

# Check for required libraries
LIB_DIR="${LIB_DIR:-./lib}"
if [[ ! -f "$LIB_DIR/libkrun.dylib" ]] && [[ ! -f "$LIB_DIR/libkrun.so" ]]; then
    echo "Error: libkrun not found in $LIB_DIR"
    echo "Set LIB_DIR to point to your libkrun library directory."
    exit 1
fi

# Build release binary
echo "Building release binary..."
LIBKRUN_BUNDLE="$LIB_DIR" cargo build --release --bin smolvm

# Sign binary (macOS only)
if [[ "$(uname -s)" == "Darwin" ]]; then
    echo "Signing binary..."
    codesign --force --sign - --entitlements smolvm.entitlements ./target/release/smolvm
fi

# Create distribution directory
echo "Creating distribution package..."
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR/lib"

# Copy binary (renamed to smolvm-bin)
cp ./target/release/smolvm "$DIST_DIR/smolvm-bin"

# Copy wrapper script
cp ./dist/smolvm "$DIST_DIR/smolvm"
chmod +x "$DIST_DIR/smolvm"

# Copy libraries
if [[ "$(uname -s)" == "Darwin" ]]; then
    cp "$LIB_DIR/libkrun.dylib" "$DIST_DIR/lib/"
    cp "$LIB_DIR/libkrunfw.4.dylib" "$DIST_DIR/lib/"
    # Create symlink for compatibility
    ln -sf libkrunfw.4.dylib "$DIST_DIR/lib/libkrunfw.dylib"
else
    cp "$LIB_DIR/libkrun.so"* "$DIST_DIR/lib/"
    cp "$LIB_DIR/libkrunfw.so"* "$DIST_DIR/lib/"
fi

# Copy README
cat > "$DIST_DIR/README.txt" << 'EOF'
smolvm - OCI-native microVM runtime

INSTALLATION
============

1. Extract this archive to a location of your choice:
   tar -xzf smolvm-*.tar.gz
   cd smolvm-*

2. (Optional) Add to PATH:
   # Add to ~/.bashrc or ~/.zshrc:
   export PATH="/path/to/smolvm-directory:$PATH"

3. (Optional) Create a symlink:
   sudo ln -s /path/to/smolvm-directory/smolvm /usr/local/bin/smolvm

USAGE
=====

Run the 'smolvm' script (not smolvm-bin directly):

  ./smolvm run alpine:latest echo "Hello World"
  ./smolvm create --name myvm alpine:latest /bin/sh
  ./smolvm start myvm
  ./smolvm list
  ./smolvm stop myvm
  ./smolvm delete myvm

REQUIREMENTS
============

- macOS 11.0+ (Apple Silicon or Intel) or Linux with KVM
- buildah (for OCI image support): brew install buildah

TROUBLESHOOTING
===============

If you see "library not found" errors, make sure you're running the
'smolvm' wrapper script, not 'smolvm-bin' directly. The wrapper sets
up the library path automatically.

For more information: https://github.com/smolvm/smolvm
EOF

# Create tarball
echo "Creating tarball..."
cd dist
tar -czf "${DIST_NAME}.tar.gz" "${DIST_NAME}"
cd ..

# Summary
echo ""
echo "Distribution package created:"
echo "  dist/${DIST_NAME}.tar.gz"
echo ""
echo "Contents:"
ls -la "$DIST_DIR"
echo ""
echo "To test locally:"
echo "  cd $DIST_DIR && ./smolvm --help"
