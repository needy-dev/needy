#!/bin/sh
# Install needy from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/needy-dev/needy/main/install/install.sh | sh
#
# Environment:
#   NEEDY_VERSION       Release tag, for example v0.0.2 [default: latest]
#   NEEDY_INSTALL_DIR   Install directory [default: $HOME/.local/bin]
#   NEEDY_DOWNLOAD_BASE Test hook: a URL that serves the release assets
#                       directly (for example a local HTTP server over a
#                       workflow run's artifacts). It replaces the GitHub
#                       release URL and ignores NEEDY_VERSION.
#
# Follows moeru-ai/auv's install/install.sh.
#
# NOTICE(install-url): needy has no domain yet, so the script is served from
# the repository's `main` branch on raw.githubusercontent.com. Move it behind
# a short URL (as auv's https://auv.moeru.ai/install.sh) once there is one.

set -eu

error() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

version=${NEEDY_VERSION:-latest}
install_dir=${NEEDY_INSTALL_DIR:-$HOME/.local/bin}
# The script works in a temporary directory, so anchor a relative path here.
case "$install_dir" in
  /*) ;;
  *) install_dir=$PWD/$install_dir ;;
esac

case "$(uname -s)" in
  Darwin) platform=apple-darwin ;;
  # The Linux binaries are static musl builds, so one archive runs on any
  # distribution whatever its glibc.
  Linux) platform=unknown-linux-musl ;;
  *) error "Unsupported operating system: $(uname -s). On Windows, download needy-x86_64-pc-windows-msvc.zip from https://github.com/needy-dev/needy/releases" ;;
esac

case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) error "Unsupported architecture: $(uname -m)" ;;
esac

# NOTICE(rosetta): a shell translated by Rosetta 2 reports x86_64 on Apple
# silicon. Install the native arm64 build there: it is the one that carries
# the guest needy for actors.
# Reference: https://developer.apple.com/documentation/apple-silicon/about-the-rosetta-translation-environment#Determine-Whether-Your-App-Is-Running-as-a-Translated-Binary
if [ "$platform" = apple-darwin ] && [ "$arch" = x86_64 ] &&
  [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
  arch=aarch64
fi

command -v curl >/dev/null 2>&1 || error 'curl is required to download needy.'

archive=needy-$arch-$platform.tar.gz
# NOTICE(release-latest): GitHub's `releases/latest` names the newest release
# that is neither a draft nor a prerelease. While every needy release is a
# prerelease, `latest` finds none; set NEEDY_VERSION to a tag then.
case "$version" in
  latest) base_url=https://github.com/needy-dev/needy/releases/latest/download ;;
  v*) base_url=https://github.com/needy-dev/needy/releases/download/$version ;;
  *) base_url=https://github.com/needy-dev/needy/releases/download/v$version ;;
esac
base_url=${NEEDY_DOWNLOAD_BASE:-$base_url}

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
cd "$work_dir"

printf 'Downloading %s\n' "$base_url/$archive" >&2
curl -fsSL "$base_url/$archive" -o "$archive" || error "Cannot download $base_url/$archive"
curl -fsSL "$base_url/$archive.sha256" -o "$archive.sha256" || error "Cannot download $base_url/$archive.sha256"

# The `.sha256` file holds the bare hex digest; `tr` drops a CR in case it was
# written with CRLF line endings.
expected=$(awk '{ print $1; exit }' "$archive.sha256" | tr -d '\r')
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$archive" | awk '{ print $1 }')
else
  actual=$(shasum -a 256 "$archive" | awk '{ print $1 }')
fi
[ "$actual" = "$expected" ] || error "Checksum mismatch for $archive: expected $expected, got $actual"
printf 'Verified %s (sha256 %s)\n' "$archive" "$actual" >&2

mkdir unpacked
tar -xzf "$archive" -C unpacked
[ -f unpacked/needy ] || error "$archive does not contain needy"
unpacked/needy --version >/dev/null || error 'The downloaded needy cannot run on this host.'

mkdir -p "$install_dir"
install -m 0755 unpacked/needy "$install_dir/needy"
printf 'Installed needy to %s\n' "$install_dir/needy" >&2

# The arm64 macOS archive also carries the guest needy, a static Linux binary
# that actors on this Mac run; needy looks for it next to itself.
for guest in unpacked/needy-linux-*; do
  [ -f "$guest" ] || continue
  install -m 0755 "$guest" "$install_dir/${guest#unpacked/}"
  printf 'Installed the guest needy to %s\n' "$install_dir/${guest#unpacked/}" >&2
done

case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) printf 'Add %s to PATH to run needy from any terminal.\n' "$install_dir" >&2 ;;
esac
printf 'Next: start a control daemon and pick a profile, see https://github.com/needy-dev/needy#getting-started\n' >&2
