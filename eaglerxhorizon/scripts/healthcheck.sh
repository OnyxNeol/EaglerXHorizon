#!/usr/bin/env bash
# EaglerXHorizon proxy healthcheck.
#
#   ./healthcheck.sh [host] [port]
#   ./healthcheck.sh proxy.example.com 443
#   HORIZON_PATH=/ws ./healthcheck.sh 127.0.0.1 8081
#
# Exit codes:
#   0  healthy: TCP open, dashboard answers 200, WebSocket upgrades with 101
#   1  bad usage
#   2  TCP connect failed
#   3  dashboard did not return 2xx
#   4  WebSocket did not upgrade
#   5  missing tooling (neither python3 nor curl available)

set -uo pipefail

HOST="${1:-127.0.0.1}"
PORT="${2:-443}"
WS_PATH="${HORIZON_PATH:-/ws}"
DASH_PATH="${HORIZON_DASH_PATH:-/}"
TIMEOUT="${HORIZON_TIMEOUT:-5}"

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
fi

if [ -z "$HOST" ] || [ -z "$PORT" ]; then
  echo "usage: $0 [host] [port]" >&2
  exit 1
fi

scheme="https"
[ "$PORT" = "80" ] && scheme="http"
if [ "$PORT" = "8081" ] || [ "$PORT" = "8000" ]; then scheme="http"; fi

say() { printf '%-14s %s\n' "$1" "$2"; }

# ----------------------------------------------------------------- 1. TCP reach
if command -v nc >/dev/null 2>&1; then
  if ! nc -z -w "$TIMEOUT" "$HOST" "$PORT" 2>/dev/null; then
    say "tcp" "FAIL ${HOST}:${PORT} unreachable"
    exit 2
  fi
else
  if ! (exec 3<>"/dev/tcp/${HOST}/${PORT}") 2>/dev/null; then
    say "tcp" "FAIL ${HOST}:${PORT} unreachable"
    exit 2
  fi
  exec 3<&- 2>/dev/null || true
fi
say "tcp" "ok ${HOST}:${PORT}"

# ------------------------------------------------------------- 2. dashboard 2xx
if command -v curl >/dev/null 2>&1; then
  code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time "$TIMEOUT" "${scheme}://${HOST}:${PORT}${DASH_PATH}" || echo 000)
  case "$code" in
    2*) say "dashboard" "ok HTTP ${code}" ;;
    *)  say "dashboard" "FAIL HTTP ${code}"; exit 3 ;;
  esac
else
  say "dashboard" "skipped (curl missing)"
fi

# -------------------------------------------------------- 3. websocket handshake
ws_check() {
  python3 - "$HOST" "$PORT" "$WS_PATH" "$TIMEOUT" <<'PY'
import base64, os, socket, ssl, sys

host, port, path, timeout = sys.argv[1], int(sys.argv[2]), sys.argv[3], float(sys.argv[4])
key = base64.b64encode(os.urandom(16)).decode()
request = (
    f"GET {path} HTTP/1.1\r\n"
    f"Host: {host}:{port}\r\n"
    "Upgrade: websocket\r\n"
    "Connection: Upgrade\r\n"
    f"Sec-WebSocket-Key: {key}\r\n"
    "Sec-WebSocket-Version: 13\r\n"
    f"Origin: https://{host}\r\n"
    "\r\n"
)

try:
    raw = socket.create_connection((host, port), timeout=timeout)
    if port in (443, 8443):
        ctx = ssl.create_default_context()
        raw = ctx.wrap_socket(raw, server_hostname=host)
    raw.sendall(request.encode())
    response = raw.recv(256).decode("latin-1", "replace")
    raw.close()
except Exception as error:  # noqa: BLE001 - report any failure the same way
    print(f"FAIL {type(error).__name__}: {error}")
    sys.exit(4)

first = response.split("\r\n", 1)[0]
if " 101" in first:
    print(f"ok {first}")
    sys.exit(0)
print(f"FAIL {first or 'empty reply'}")
sys.exit(4)
PY
}

if command -v python3 >/dev/null 2>&1; then
  if output=$(ws_check); then
    say "websocket" "$output"
  else
    say "websocket" "$output"
    exit 4
  fi
elif command -v curl >/dev/null 2>&1; then
  # curl cannot complete a WS handshake, but a 101 in the headers is enough.
  headers=$(curl -sS -i -N --max-time "$TIMEOUT" \
    -H "Connection: Upgrade" -H "Upgrade: websocket" \
    -H "Sec-WebSocket-Version: 13" \
    -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==" \
    -H "Origin: ${scheme}://${HOST}" \
    "${scheme}://${HOST}:${PORT}${WS_PATH}" 2>/dev/null | head -n 1)
  case "$headers" in
    *101*) say "websocket" "ok ${headers}" ;;
    *)     say "websocket" "FAIL ${headers:-no reply}"; exit 4 ;;
  esac
else
  say "websocket" "skipped (python3 and curl both missing)"
  exit 5
fi

say "result" "healthy"
exit 0
