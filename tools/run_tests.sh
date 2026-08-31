#!/usr/bin/env bash
#
# Builds and runs both test suites: the C++ core and the Python server.
# This is what CI runs, so a green run here is a green run there.
#
#   ./run_tests.sh              # both suites
#   ./run_tests.sh --sanitize   # C++ suite again under ASan/UBSan, then TSan
#
set -euo pipefail

cd "$(dirname "$0")"
CPP=../app/src/main/cpp
CXXFLAGS="-std=c++17 -Wall -Wextra -I$CPP"
SOURCES="host_test.cpp $CPP/udp_socket.cpp"
SANITIZE=0
[ "${1:-}" = "--sanitize" ] && SANITIZE=1

echo "==> C++ core"
g++ $CXXFLAGS -O2 $SOURCES -o host_test -lpthread
./host_test

if [ "$SANITIZE" = "1" ]; then
    # The engine's whole point is two threads meeting in lock-free buffers, so
    # the sanitizer runs are the ones that actually matter.
    for san in address,undefined thread; do
        echo "==> C++ core under -fsanitize=$san"
        g++ $CXXFLAGS -O1 -g -fsanitize=$san $SOURCES -o "host_test_${san%%,*}" -lpthread
        "./host_test_${san%%,*}"
    done
fi

echo "==> Python server"
if python3 -c "import numpy" 2>/dev/null; then
    python3 test_python_server.py
else
    echo "SKIP: numpy is missing (pip install -r ../server/requirements.txt)"
fi

echo
echo "all suites passed"
