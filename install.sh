#!/usr/bin/env bash
set -euo pipefail

source_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
data_home=${XDG_DATA_HOME:-$HOME/.local/share}
config_home=${XDG_CONFIG_HOME:-$HOME/.config}
install_directory="$data_home/discord-nvenc-nvdec-patch"
desktop_directory="$data_home/applications"
desktop_file="$desktop_directory/discord.desktop"
backup_file="$install_directory/original.discord.desktop"
system_desktop=/usr/share/applications/discord.desktop
discord_host="$config_home/discord/Discord"
marker='X-Discord-Nvenc-Nvdec-Installer=true'

is_installed_desktop() {
    [[ -f "$desktop_file" ]] && grep -Fxq -- "$marker" "$desktop_file"
}

case ${1:-install} in
    install)
        if [[ ! -x "$discord_host" ]]; then
            printf 'Discord host not found: %s\n' "$discord_host" >&2
            exit 1
        fi
        if [[ ! -f "$source_directory/libdiscord_nvenc_nvdec.so" || ! -f "$source_directory/launch.sh" ]]; then
            printf 'Patch library or launcher not found. Run build.sh first.\n' >&2
            exit 1
        fi
        if [[ ! -f "$system_desktop" ]]; then
            printf 'System Discord desktop entry not found: %s\n' "$system_desktop" >&2
            exit 1
        fi
        if [[ "$install_directory" == *[$'\n'\\\"%]* ]]; then
            printf 'Installation path contains a character unsupported by this desktop launcher.\n' >&2
            exit 1
        fi
        if [[ -e "$backup_file" ]] && ! is_installed_desktop; then
            printf 'An earlier desktop backup exists and the current entry is not managed by this installer.\n' >&2
            exit 1
        fi

        install -d -m 700 -- "$install_directory"
        install -d -m 700 -- "$desktop_directory"
        if [[ -e "$desktop_file" ]] && ! is_installed_desktop; then
            cp -p -- "$desktop_file" "$backup_file"
        fi

        install -m 600 -- "$source_directory/libdiscord_nvenc_nvdec.so" "$install_directory/libdiscord_nvenc_nvdec.so"
        install -m 700 -- "$source_directory/launch.sh" "$install_directory/launch.sh"

        temporary_desktop=$(mktemp --suffix=.desktop -- "$desktop_directory/.discord-nvenc-nvdec.XXXXXX")
        trap 'rm -f -- "$temporary_desktop"' EXIT
        awk -v executable="\"$install_directory/launch.sh\"" -v marker="$marker" '
            /^Exec=/ { print "Exec=" executable; replaced = 1; next }
            { print }
            END {
                if (!replaced) exit 1
                print marker
            }
        ' "$system_desktop" > "$temporary_desktop"
        if command -v desktop-file-validate >/dev/null 2>&1; then
            desktop-file-validate "$temporary_desktop"
        fi
        chmod 600 -- "$temporary_desktop"
        mv -f -- "$temporary_desktop" "$desktop_file"
        trap - EXIT
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database "$desktop_directory" >/dev/null 2>&1 || true
        fi
        printf 'Installed for the next Discord launch: %s\n' "$desktop_file"
        ;;
    uninstall)
        if ! is_installed_desktop; then
            printf 'Managed Discord desktop entry not found; no files changed.\n' >&2
            exit 1
        fi
        if [[ -f "$backup_file" ]]; then
            mv -- "$backup_file" "$desktop_file"
        else
            rm -- "$desktop_file"
        fi
        if [[ -d "$install_directory" ]]; then
            rm -f -- "$install_directory/libdiscord_nvenc_nvdec.so" "$install_directory/launch.sh"
            rmdir -- "$install_directory" 2>/dev/null || true
        fi
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database "$desktop_directory" >/dev/null 2>&1 || true
        fi
        printf 'Removed the Discord patch launcher.\n'
        ;;
    *)
        printf 'Usage: %s [install|uninstall]\n' "$0" >&2
        exit 2
        ;;
esac
