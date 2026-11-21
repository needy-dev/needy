#!/bin/sh
# Codex, the static musl build, as the release's package: the codex binary
# with its helpers (codex-code-mode-host, codex-exec-server, rg, bwrap) and
# codex-package.json.
# Source: https://github.com/openai/codex/releases/tag/rust-v0.167.2
# (`codex-package-<target>.tar.gz`; each asset's `digest` in the GitHub
# release API, as in the release's codex-package_SHA256SUMS, retrieved 2026-11-13)
# Reference: https://github.com/openai/codex/blob/rust-v0.167.2/README.md
set -eu

VERSION="${VERSION:-0.167.2}"
PINNED_VERSION="0.167.2"
SHA256_AMD64="3d77be1748d35a4bb0e7f8752cde3b11890c63338e8c599f01eafa4a59e17b3d"
SHA256_ARM64="56b08c1214e062a5b61684bacb5033c4372a01062a47c87c49a32be83af70695"

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
  x86_64|amd64) target="x86_64-unknown-linux-musl"; sum="$SHA256_AMD64" ;;
  aarch64|arm64) target="aarch64-unknown-linux-musl"; sum="$SHA256_ARM64" ;;
  *) echo "codex feature: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

curl -fsSL -o /tmp/codex.tar.gz \
  "https://github.com/openai/codex/releases/download/rust-v${VERSION}/codex-package-${target}.tar.gz"
if [ "$VERSION" = "$PINNED_VERSION" ]; then
  echo "${sum}  /tmp/codex.tar.gz" | sha256sum -c -
fi

rm -rf /opt/codex
mkdir -p /opt/codex
tar -xzf /tmp/codex.tar.gz -C /opt/codex --no-same-owner
rm -f /tmp/codex.tar.gz
ln -sf /opt/codex/bin/codex /usr/local/bin/codex

if [ -n "${_REMOTE_USER:-}" ] && [ -n "${_REMOTE_USER_HOME:-}" ] && [ "$_REMOTE_USER" != "root" ]; then
  mkdir -p "${_REMOTE_USER_HOME}/.codex"
  chown "${_REMOTE_USER}:${_REMOTE_USER}" "${_REMOTE_USER_HOME}/.codex" 2>/dev/null || true
fi

codex --version
