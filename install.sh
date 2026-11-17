#!/bin/sh
# Install a published LazyMerge binary. No Rust toolchain or root access needed.
# Optional: LAZYMERGE_VERSION=0.1.1 LAZYMERGE_INSTALL_DIR=/your/bin sh install.sh
set -eu

fail() { printf 'LazyMerge: %s\n' "$*" >&2; exit 1; }

download() {
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
            --retry 3 --output "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget --https-only --quiet --output-document="$2" "$1"
    else
        fail 'Install curl or wget, then rerun the installer.'
    fi
}

case "$(uname -s)" in
    Linux) platform=unknown-linux-musl ;;
    Darwin) platform=apple-darwin ;;
    *) fail 'This installer supports Linux and macOS. On Windows, use install.ps1.' ;;
esac
machine=$(uname -m)
# Prefer the native Apple Silicon binary when launched through Rosetta.
if [ "$platform" = apple-darwin ] && [ "$machine" = x86_64 ] \
    && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
    machine=arm64
fi
case "$machine" in
    x86_64|amd64) architecture=x86_64 ;;
    aarch64|arm64) architecture=aarch64 ;;
    *) fail "Unsupported architecture: $machine (supported: x86_64 and ARM64)." ;;
esac
target="$architecture-$platform"
archive="lazymerge-$target.tar.gz"
version=${LAZYMERGE_VERSION:-}
if [ -n "$version" ]; then
    version=${version#v}
    printf '%s\n' "$version" | LC_ALL=C grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][A-Za-z0-9.-]+)?$' \
        || fail 'LAZYMERGE_VERSION must be a version such as 0.1.1.'
    release="https://github.com/he-d/lazymerge/releases/download/v$version"
else
    release=https://github.com/he-d/lazymerge/releases/latest/download
fi

command -v tar >/dev/null 2>&1 || fail 'The tar command is required.'
if command -v sha256sum >/dev/null 2>&1; then
    checksum=sha256sum
elif command -v shasum >/dev/null 2>&1; then
    checksum=shasum
else
    fail 'Install sha256sum or shasum to verify the download.'
fi

temporary=$(mktemp -d "${TMPDIR:-/tmp}/lazymerge-install.XXXXXX")
staged=
cleanup() {
    [ -z "$staged" ] || rm -f "$staged"
    rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM
printf 'Downloading LazyMerge for %s…\n' "$target"
download "$release/$archive" "$temporary/$archive" \
    || fail "Download failed. Check that a release for $target has been published."
download "$release/SHA256SUMS" "$temporary/SHA256SUMS" \
    || fail 'Could not download release checksums.'
expected=$(awk -v asset="$archive" '
    { name = $2; sub(/^\*/, "", name); if (name == asset) { print $1; count++ } }
    END { if (count != 1) exit 1 }
' "$temporary/SHA256SUMS") || fail "No unique checksum was found for $archive."
case "$expected" in *[!0-9a-fA-F]*|'') fail 'The release checksum is invalid.' ;; esac
[ "${#expected}" -eq 64 ] || fail 'The release checksum is invalid.'
if [ "$checksum" = sha256sum ]; then
    actual=$(sha256sum "$temporary/$archive" | awk '{ print $1 }')
else
    actual=$(shasum -a 256 "$temporary/$archive" | awk '{ print $1 }')
fi
expected=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
[ "$actual" = "$expected" ] || fail 'Checksum mismatch; the existing installation was not changed.'

# Extract only the executable, never unrelated files from the archive.
tar -xzf "$temporary/$archive" -C "$temporary" lazymerge \
    || fail 'The release archive could not be unpacked.'
[ -f "$temporary/lazymerge" ] && [ ! -L "$temporary/lazymerge" ] \
    || fail 'The archive does not contain a regular lazymerge executable.'
[ -s "$temporary/lazymerge" ] || fail 'The release executable is empty.'
install_dir=${LAZYMERGE_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}
mkdir -p "$install_dir"
install_dir=$(CDPATH= cd "$install_dir" && pwd -P)
[ ! -d "$install_dir/lazymerge" ] || fail "$install_dir/lazymerge is a directory."
# Stage beside the destination so an update is an atomic rename, even if it is running.
staged=$(mktemp "$install_dir/.lazymerge.XXXXXX")
cat "$temporary/lazymerge" > "$staged"
chmod 755 "$staged"
mv -f "$staged" "$install_dir/lazymerge"
staged=
printf 'Installed LazyMerge to %s/lazymerge\n' "$install_dir"
current=$(command -v lazymerge 2>/dev/null || true)
if [ "$current" = "$install_dir/lazymerge" ]; then
    printf 'Run: lazymerge\n'
else
    if [ -n "$current" ]; then
        printf 'Another lazymerge currently takes precedence: %s\n' "$current"
    fi
    printf 'Prepend %s to your PATH, then run lazymerge.\n' "$install_dir"
    # Quote directory names so the guidance also works with spaces or apostrophes.
    quoted=$(printf '%s' "$install_dir" | sed "s/'/'\\\\''/g")
    printf "For this shell: export PATH='%s':\"\$PATH\"\n" "$quoted"
fi
