#!/usr/bin/env bash
set -euo pipefail

directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
discord_host="$config_home/discord/Discord"

if [[ ! -x "$discord_host" ]]; then
    printf 'Discord host not found: %s\n' "$discord_host" >&2
    exit 1
fi
if [[ ! -f "$directory/libdiscord_nvenc_nvdec.so" ]]; then
    printf 'Patch library not found. Run build.sh first.\n' >&2
    exit 1
fi

export LD_PRELOAD="$directory/libdiscord_nvenc_nvdec.so${LD_PRELOAD:+:$LD_PRELOAD}"
exec "$discord_host" "$@"
