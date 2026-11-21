#!/bin/sh
# herdr, which runs agents in panes.
# Source: https://github.com/herdrdev/herdr/releases/tag/v0.9.5 (each asset's
# `digest`; retrieved 2026-11-13)
# Reference: https://github.com/herdrdev/herdr/blob/v0.9.5/README.md
set -eu

VERSION="${VERSION:-0.9.5}"
PINNED_VERSION="0.9.5"
SHA256_AMD64="14d9e8bc8b4a9e0118d77c32266f76d461f09b40342eb23752960bd89f59929d"
SHA256_ARM64="17d7536f2da50859523680ef279dffcac8e4a235f4e46fe8da6c2ec44141897c"

if ! command -v curl >/dev/null 2>&1; then
  if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends curl ca-certificates
    rm -rf /var/lib/apt/lists/*
  elif command -v apk >/dev/null 2>&1; then
    apk add --no-cache curl ca-certificates
  fi
fi

case "$(uname -m)" in
  x86_64|amd64) arch="x86_64"; sum="$SHA256_AMD64" ;;
  aarch64|arm64) arch="aarch64"; sum="$SHA256_ARM64" ;;
  *) echo "herdr feature: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

curl -fsSL -o /usr/local/bin/herdr \
  "https://github.com/herdrdev/herdr/releases/download/v${VERSION}/herdr-linux-${arch}"
if [ "$VERSION" = "$PINNED_VERSION" ]; then
  echo "${sum}  /usr/local/bin/herdr" | sha256sum -c -
fi
chmod 0755 /usr/local/bin/herdr

herdr --version
