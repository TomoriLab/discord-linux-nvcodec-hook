#!/usr/bin/env bash
set -euo pipefail

directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd "$directory"
cargo build --release --offline --locked
cp -- target/release/libdiscord_nvenc_nvdec.so "$directory/libdiscord_nvenc_nvdec.so"
