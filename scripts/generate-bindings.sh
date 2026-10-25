#!/usr/bin/env bash
set -euo pipefail

directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
generated=$(mktemp)
trap 'rm -f -- "$generated"' EXIT

RUST_LOG=error bindgen "$directory/wrapper.h" \
    --allowlist-function '^(cuInit|cuDeviceGet|cuCtxCreate_v2|cuCtxDestroy_v2|cuCtxPushCurrent_v2|cuCtxPopCurrent_v2|cuMemcpy2D_v2|cuvid.*)$' \
    --allowlist-type '^(CU|cuda).*' \
    --allowlist-var '^(CU|cuda).*' \
    --no-layout-tests \
    --output "$generated" \
    -- "-I$directory/include"

{
    printf '%s\n' '// Generated with bindgen 0.72.1 from FFmpeg nv-codec-headers commit f2053071ea93137255b6dcf42e9b4c9e0965a44e.'
    printf '%s\n' '// Source copyright and permission notices: include/ffnvcodec/*.h and THIRD_PARTY_NOTICES.md.'
    printf '%s\n' '#![allow(dead_code, unused_imports, clippy::all)]'
    cat "$generated"
} > "$directory/src/ffi.rs"
rustfmt --edition 2024 "$directory/src/ffi.rs"
