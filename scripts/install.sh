#!/bin/sh
set -eu

usage() {
  cat <<'EOF'
Usage: ./scripts/install.sh --prefix DIRECTORY [--skip-image] [--image NAME]

Build and install Needy to DIRECTORY/bin/needy. Requires Rust and Cargo.
Builds the local runtime image using Docker unless --skip-image is set.
Does not install Docker or a background service.
EOF
}

prefix=
image=needy-runtime:rust
build_image=1
while [ "$#" -gt 0 ]; do
  case "$1" in
    --prefix)
      [ "$#" -ge 2 ] && [ -n "$2" ] || { usage >&2; exit 2; }
      prefix=$2
      shift 2
      ;;
    --image)
      [ "$#" -ge 2 ] && [ -n "$2" ] || { usage >&2; exit 2; }
      image=$2
      shift 2
      ;;
    --skip-image)
      build_image=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown option: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[ -n "$prefix" ] || { usage >&2; exit 2; }
case "$prefix" in
  /*) ;;
  *) prefix="$PWD/$prefix" ;;
esac
command -v cargo >/dev/null 2>&1 || { printf 'Rust and Cargo are required to build Needy.\n' >&2; exit 1; }
if [ "$build_image" -eq 1 ]; then
  command -v docker >/dev/null 2>&1 || { printf 'Docker is required to build the runtime image.\n' >&2; exit 1; }
  docker info >/dev/null || { printf 'Docker is not available. Start the Docker backend first.\n' >&2; exit 1; }
fi

source_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$source_dir"
cargo build --locked --release --bin needy
if [ "$build_image" -eq 1 ]; then
  docker build -t "$image" -f images/Dockerfile .
fi

mkdir -p "$prefix/bin"
install -m 0755 "${CARGO_TARGET_DIR:-target}/release/needy" "$prefix/bin/needy"
printf 'Installed %s/bin/needy\nRuntime image: %s\n' "$prefix" "$image"
