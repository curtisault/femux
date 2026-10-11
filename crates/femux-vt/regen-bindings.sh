#!/usr/bin/env bash
# Regenerates src/ffi.rs from the pinned libghostty-vt headers.
# Run after bumping the ghostty pin in zig/build.zig.zon.
# Needs: bindgen-cli (`cargo install bindgen-cli`), libclang, zig 0.16.
set -euo pipefail
cd "$(dirname "$0")"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
(cd zig && zig build -Doptimize=ReleaseFast --prefix "$out")
bindgen "$out/include/ghostty/vt.h" \
  --allowlist-function 'ghostty_.*' \
  --allowlist-type 'Ghostty.*' \
  --allowlist-var 'GHOSTTY_.*' \
  --default-enum-style newtype \
  --use-core \
  --raw-line '#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code, clippy::all)]' \
  -o src/ffi.rs \
  -- -I"$out/include" -DGHOSTTY_STATIC
