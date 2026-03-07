#!/usr/bin/env bash

set -Eeuo pipefail

echo "========================================"
echo " RustChan Development Check"
echo "========================================"

# ----------------------------------------
# Helper functions
# ----------------------------------------

command_exists () {
	command -v "$1" >/dev/null 2>&1
}

step () {
	echo ""
	echo "---- $1 ----"
}

# ----------------------------------------
# Use all CPU cores for compilation
# ----------------------------------------

if command_exists sysctl; then
	export CARGO_BUILD_JOBS=$(sysctl -n hw.ncpu)
else
	export CARGO_BUILD_JOBS=$(nproc || echo 4)
fi

echo "Using $CARGO_BUILD_JOBS CPU cores for builds"

# ----------------------------------------
# Verify required tools
# ----------------------------------------

step "Checking required tools"

required_tools=(
	cargo
	rustfmt
	clippy-driver
)

for tool in "${required_tools[@]}"; do
	if ! command_exists "$tool"; then
		echo "Error: $tool is not installed."
		exit 1
	fi
done

# Optional tools
if ! command_exists cargo-audit; then
	echo "Warning: cargo-audit not installed."
	echo "Install with: cargo install cargo-audit"
fi

if ! command_exists cargo-deny; then
	echo "Warning: cargo-deny not installed."
	echo "Install with: cargo install cargo-deny"
fi

# ----------------------------------------
# Update dependencies (optional)
# ----------------------------------------

if [[ "${1:-}" == "--update" ]]; then
	step "Updating dependency index"
	cargo update
fi

# ----------------------------------------
# Format code
# ----------------------------------------

step "Formatting code"

cargo fmt --all

# ----------------------------------------
# Apply automatic fixes
# ----------------------------------------

step "Applying automatic fixes"

cargo fix --allow-dirty --allow-staged --allow-no-vcs

# ----------------------------------------
# Run clippy
# ----------------------------------------

step "Running clippy"

cargo clippy \
	--all-targets \
	--all-features \
	-- -D warnings

# ----------------------------------------
# Run tests
# ----------------------------------------

step "Running tests"

cargo test --all --all-features

# ----------------------------------------
# Security audit (RustSec)
# ----------------------------------------

if command_exists cargo-audit; then
	step "Running cargo-audit"
	cargo audit
fi

# ----------------------------------------
# Dependency policy / security checks
# ----------------------------------------

if command_exists cargo-deny; then
	step "Running cargo-deny"
	cargo deny check
fi

# ----------------------------------------
# Dependency duplication check
# ----------------------------------------

step "Checking for duplicate dependencies"

cargo tree -d || true

# ----------------------------------------
# Done
# ----------------------------------------

echo ""
echo "========================================"
echo " All checks passed successfully"
echo "========================================"