#!/usr/bin/env bash
#
# Touchbard installer
#
#   A single-window UI runtime for a Touch Bar / hyper-strip display,
#   plus the Control Center application that runs on it (Linux only).
#
# This script:
#   1. Ensures a Rust toolchain is available (installs rustup if missing)
#   2. Builds the workspace in release mode
#   3. Prints run instructions for the preview and DRM backends
#
# Usage:
#   curl -fsSL https://<user>.github.io/touchbard/install.sh | bash
#   bash docs/install.sh [--release|--debug]
#
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${1:---release}"
[[ "$PROFILE" == "--release" || "$PROFILE" == "--debug" ]] || {
    echo "error: expected --release or --debug, got '$PROFILE'" >&2
    exit 2
}

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m==>\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m==>\033[0m %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Step 1: Rust toolchain
# ---------------------------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
    say "cargo not found; installing Rust via rustup..."
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- https://sh.rustup.rs | sh -s -- -y --profile minimal
    else
        die "need curl or wget to install rustup (or install Rust manually)"
    fi
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env" 2>/dev/null || true
fi
command -v cargo >/dev/null 2>&1 || die "cargo still not found after install"
say "using $(cargo --version) at $(command -v cargo)"

# ---------------------------------------------------------------------------
# Step 2: Build
# ---------------------------------------------------------------------------
cd "$REPO_DIR"
say "building workspace ($PROFILE)..."
cargo build "$PROFILE" --workspace

# ---------------------------------------------------------------------------
# Step 3: Run instructions
# ---------------------------------------------------------------------------
cat <<EOF

Touchbard built successfully.

Controls:

  Browser preview (no hardware required):
    (cd $REPO_DIR && cargo run $PROFILE --example control-center -- --preview)
    then open http://127.0.0.1:8888

  Physical Touch Bar / hyper-strip via DRM/KMS:
    (cd $REPO_DIR && cargo run $PROFILE --example control-center -- --drm)

  Tests:
    (cd $REPO_DIR && cargo test --workspace)

Tip: if the DRM backend cannot open /dev/dri nodes, run as root or add a
udev rule granting the 'render'/'video' groups access to the device;
the existing KMS connector (2008x60 landscape) is auto-detected at runtime.
EOF