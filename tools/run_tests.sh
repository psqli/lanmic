#!/usr/bin/env bash
#
# Builds and runs every test suite: the Rust engine, the desktop app, and the
# Python server.
# This is what CI runs, so a green run here is a green run there.
#
#   ./run_tests.sh              # every suite
#   ./run_tests.sh --strict     # also clippy and rustfmt, as CI does
#
# The desktop app is skipped when its system libraries are missing, the same
# way the Python suite is skipped without numpy: an engine change should still
# be testable on a machine with no ALSA headers.
#
set -euo pipefail

cd "$(dirname "$0")"
RUST=../rust
DESKTOP=../desktop
STRICT=0
[ "${1:-}" = "--strict" ] && STRICT=1

echo "==> Rust engine"
# The whole engine except the two Oboe streams is portable, so this covers the
# protocol, the ring, the jitter buffer, the meter, the mixer, the limiter, and
# a real UDP loopback from capture through to a sample-accurate mix.
cargo test --manifest-path $RUST/Cargo.toml --locked

if [ "$STRICT" = "1" ]; then
    echo "==> rustfmt"
    cargo fmt --manifest-path $RUST/Cargo.toml --check
    echo "==> clippy"
    cargo clippy --manifest-path $RUST/Cargo.toml --all-targets --locked -- -D warnings
fi

echo "==> Desktop app"
# gpui needs xkbcommon and the Wayland/X11 headers; cpal needs ALSA. On Debian
# and Ubuntu that is libasound2-dev libxkbcommon-dev libwayland-dev libx11-dev
# libxcb1-dev libfontconfig1-dev.
if pkg-config --exists alsa xkbcommon wayland-client x11 2>/dev/null; then
    cargo test --manifest-path $DESKTOP/Cargo.toml --locked
    if [ "$STRICT" = "1" ]; then
        cargo fmt --manifest-path $DESKTOP/Cargo.toml --check
        cargo clippy --manifest-path $DESKTOP/Cargo.toml --all-targets --locked -- -D warnings
    fi
else
    echo "SKIP: system libraries for gpui/cpal are missing"
fi

echo "==> Python server"
if python3 -c "import numpy" 2>/dev/null; then
    python3 test_python_server.py
else
    echo "SKIP: numpy is missing (pip install -r ../server/requirements.txt)"
fi

echo
echo "all suites passed"
