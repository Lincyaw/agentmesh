#!/bin/sh
# Download a pinned, checksummed binary; optional private bootstrap enrollment.
set -eu
umask 077
version=${AGENTMESH_VERSION:-0.5.0}
join=0
no_service=0
code_file=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --join) join=1 ;;
        --no-service) no_service=1 ;;
        --code-file) shift; code_file=${1:?--code-file requires a path}; join=1 ;;
        --help) printf '%s\n' 'Usage: install.sh [--join] [--code-file PRIVATE_FILE] [--no-service]'; exit 0 ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done
case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) platform=linux-x64 ;;
    Linux/aarch64|Linux/arm64) platform=linux-arm64 ;;
    Darwin/arm64) platform=macos-arm64 ;;
    *) printf '%s\n' 'Supported: Linux x64/ARM64 and macOS ARM64.' >&2; exit 7 ;;
esac
for tool in curl tar mktemp; do command -v "$tool" >/dev/null 2>&1 || { printf 'Missing dependency: %s\n' "$tool" >&2; exit 7; }; done
case "$version" in *[!0-9.]*|'') printf '%s\n' 'Invalid AGENTMESH_VERSION.' >&2; exit 2 ;; esac
temporary=$(mktemp -d "${TMPDIR:-/tmp}/agentmesh-install.XXXXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
archive="agentmesh-v$version-$platform.tar.gz"
base=${AGENTMESH_DOWNLOAD_BASE:-https://github.com/Lincyaw/agentmesh/releases/download/v$version}
case "$base" in https://*) ;; *) printf '%s\n' 'Download base must use HTTPS.' >&2; exit 2 ;; esac
printf 'Downloading Agentmesh %s (%s)...\n' "$version" "$platform" >&2
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 --retry 2 "$base/$archive" -o "$temporary/$archive"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 --retry 2 "$base/SHA256SUMS" -o "$temporary/SHA256SUMS"
expected=$(awk -v file="$archive" '$2==file {print $1}' "$temporary/SHA256SUMS")
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temporary/$archive" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$temporary/$archive" | awk '{print $1}')
else
    printf '%s\n' 'Install sha256sum or shasum before running the installer.' >&2; exit 7
fi
if [ "${#expected}" -ne 64 ] || [ "$expected" != "$actual" ]; then printf '%s\n' 'SHA-256 verification failed; nothing installed.' >&2; exit 4; fi
tar -xzf "$temporary/$archive" -C "$temporary" "agentmesh-v$version-$platform/agentmesh"
binary="$temporary/agentmesh-v$version-$platform/agentmesh"
chmod 700 "$binary"
if [ -n "$code_file" ]; then
    AGENTMESH_INVITE_CODE=$(cat "$code_file")
    export AGENTMESH_INVITE_CODE
fi
if [ -n "${AGENTMESH_INVITE_CODE:-}" ]; then join=1; fi
if [ "$join" -eq 1 ]; then
    if [ -z "${AGENTMESH_INVITE_CODE:-}" ]; then
        printf '%s' 'Paste your private bootstrap invitation: ' >&2
        IFS= read -r AGENTMESH_INVITE_CODE </dev/tty || { printf '\n%s\n' 'No invitation provided.' >&2; exit 2; }
        export AGENTMESH_INVITE_CODE
    fi
    # join validates the invitation before installing or modifying configuration.
    if [ "$no_service" -eq 1 ]; then "$binary" join --no-service; else "$binary" join; fi
else
    "$binary" setup
fi
bin_dir=${AGENTMESH_BIN_DIR:-$HOME/.local/bin}
mkdir -p "$bin_dir"
cp "$binary" "$bin_dir/.agentmesh-install.$$"
chmod 755 "$bin_dir/.agentmesh-install.$$"
mv -f "$bin_dir/.agentmesh-install.$$" "$bin_dir/agentmesh"
printf 'Installed: %s/agentmesh\nLaunch claude, tclaude, codex or tcodex normally. Reconnect existing MCP sessions.\n' "$bin_dir" >&2
