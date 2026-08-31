#!/usr/bin/env bash
#
# Builds and runs both test suites: the Rust engine and the Python server.
# This is what CI runs, so a green run here is a green run there.
#
#   ./run_tests.sh              # both suites
#   ./run_tests.sh --strict     # also clippy and rustfmt, as CI does
#
set -euo pipefail

cd "$(dirname "$0")"
RUST=../rust
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

echo "==> Python server"
if python3 -c "import numpy" 2>/dev/null; then
    python3 test_python_server.py
else
    echo "SKIP: numpy is missing (pip install -r ../server/requirements.txt)"
fi

echo
echo "all suites passed"
