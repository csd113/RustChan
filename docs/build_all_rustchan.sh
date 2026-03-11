#!/bin/bash
set -e

########################################
# CONFIG
########################################

APP_NAME="rustchan-cli"
VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d '"' -f2)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/target/release-bundles"
STAGING="$BUILD_DIR/staging"
DOWNLOADS="$HOME/Downloads"

mkdir -p "$BUILD_DIR"
rm -rf "$STAGING"
mkdir -p "$STAGING"

echo "========================================"
echo "Building $APP_NAME v$VERSION"
echo "========================================"

########################################
# Build macOS
########################################

build_macos() {
	ARCH=$1
	TARGET="${ARCH}-apple-darwin"
	BINNAME="${APP_NAME}-macos"
	ARCHIVE="${APP_NAME}-macos-v${VERSION}-${ARCH}.tar.gz"

	echo "Building macOS $ARCH..."

	cargo build --release --target "$TARGET" -j 2

	STAGE="$STAGING/macos-$ARCH"
	mkdir -p "$STAGE"

	cp "target/${TARGET}/release/${APP_NAME}" "$STAGE/$BINNAME"

	strip "$STAGE/$BINNAME" 2>/dev/null || true

	tar -czf "$BUILD_DIR/$ARCHIVE" -C "$STAGE" "$BINNAME"
}

########################################
# Build Linux
########################################

build_linux() {
	ARCH=$1
	TARGET="${ARCH}-unknown-linux-musl"
	BINNAME="${APP_NAME}-linux"
	ARCHIVE="${APP_NAME}-linux-v${VERSION}-${ARCH}.tar.gz"

	echo "Building Linux $ARCH..."

	cargo zigbuild --release --target "$TARGET" -j 2

	STAGE="$STAGING/linux-$ARCH"
	mkdir -p "$STAGE"

	cp "target/${TARGET}/release/${APP_NAME}" "$STAGE/$BINNAME"

	strip "$STAGE/$BINNAME" 2>/dev/null || true

	tar -czf "$BUILD_DIR/$ARCHIVE" -C "$STAGE" "$BINNAME"
}

########################################
# Build Windows
########################################

build_windows() {

	ARCH="x86_64"
	TARGET="x86_64-pc-windows-gnu"
	BINNAME="${APP_NAME}-windows.exe"
	ARCHIVE="${APP_NAME}-windows-v${VERSION}-${ARCH}.zip"

	echo "Building Windows..."

	cargo build --release --target "$TARGET" -j 2

	STAGE="$STAGING/windows-$ARCH"
	mkdir -p "$STAGE"

	cp "target/${TARGET}/release/${APP_NAME}.exe" "$STAGE/$BINNAME"

	cd "$STAGE"

	zip "$BUILD_DIR/$ARCHIVE" "$BINNAME"

	cd "$SCRIPT_DIR"
}

########################################
# Run Builds
########################################

build_macos aarch64
build_macos x86_64
build_linux x86_64
build_linux aarch64
build_windows

########################################
# Checksums
########################################

echo "Generating checksums..."

cd "$BUILD_DIR"

FILES=$(ls *.tar.gz *.zip 2>/dev/null)

if [ -z "$FILES" ]; then
	echo "Error: No release files found"
	exit 1
fi

shasum -a 256 *.tar.gz *.zip > "${APP_NAME}-v${VERSION}-SHA256SUMS.txt"

cd "$SCRIPT_DIR"

########################################
# Copy to Downloads
########################################

echo "Copying artifacts to Downloads..."

cp "$BUILD_DIR"/*.tar.gz "$BUILD_DIR"/*.zip "$BUILD_DIR"/*.txt "$DOWNLOADS/" 2>/dev/null || true

echo "========================================"
echo "Build complete!"
echo "Artifacts available in:"
echo "$BUILD_DIR"
echo "and copied to:"
echo "$DOWNLOADS"
echo "========================================"