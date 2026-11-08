#!/usr/bin/env bash
# Build the Rust engine to WebAssembly and place the bundle where the dashboard
# expects it: web/pkg/eaglerxhorizon.js + eaglerxhorizon_bg.wasm
#
#   ./scripts/build-wasm.sh            # release build
#   ./scripts/build-wasm.sh --debug    # faster, larger, unoptimised
#
# Requires: rustup target add wasm32-unknown-unknown, wasm-pack, wasm-opt (optional)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/web/pkg"
TARGET="wasm32-unknown-unknown"
MODE="release"

for arg in "$@"; do
  case "$arg" in
    --debug) MODE="debug" ;;
    -h|--help)
      sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing dependency: $1" >&2
    echo "  install wasm-pack:  curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh" >&2
    exit 1
  }
}

need rustup
need wasm-pack

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "adding target $TARGET"
  rustup target add "$TARGET"
fi

cd "$ROOT"

ARGS=(build --target web --out-dir "$OUT" --out-name eaglerxhorizon --no-typescript)
if [ "$MODE" = "release" ]; then
  ARGS+=(--release)
fi

echo "==> wasm-pack ${ARGS[*]}"
wasm-pack "${ARGS[@]}"

# Shrink the binary when wasm-opt is available; it is a big win for wgpu.
if command -v wasm-opt >/dev/null 2>&1 && [ "$MODE" = "release" ]; then
  echo "==> wasm-opt -Oz"
  wasm-opt -Oz "$OUT/eaglerxhorizon_bg.wasm" -o "$OUT/eaglerxhorizon_bg.wasm.opt"
  mv "$OUT/eaglerxhorizon_bg.wasm.opt" "$OUT/eaglerxhorizon_bg.wasm"
else
  echo "==> wasm-opt not found, skipping (install binaryen for a smaller bundle)"
fi

SIZE=$(du -h "$OUT/eaglerxhorizon_bg.wasm" | cut -f1)
echo
echo "built: $OUT"
echo "  eaglerxhorizon.js       (glue)"
echo "  eaglerxhorizon_bg.wasm  ($SIZE)"
echo
echo "serve it:  python3 -m http.server 8000 --directory web"
echo "open:      http://localhost:8000/"
