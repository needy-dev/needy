#!/bin/sh
# opencode coding agent.
# Source: https://github.com/sst/opencode/releases/tag/v1.19.5
# (matches `OPENCODE_VERSION=v1.19.5` in `images/Dockerfile`)
# Reference: https://github.com/sst/opencode
set -eu

VERSION="${VERSION:-1.19.5}"

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
  x86_64|amd64) arch="x64" ;;
  aarch64|arm64) arch="arm64" ;;
  *) echo "opencode feature: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

curl -fsSL "https://github.com/sst/opencode/releases/download/v${VERSION}/opencode-linux-${arch}.tar.gz" \
  | tar -xz -C /usr/local/bin opencode
chmod 0755 /usr/local/bin/opencode

if [ -n "${_REMOTE_USER:-}" ] && [ -n "${_REMOTE_USER_HOME:-}" ] && [ "$_REMOTE_USER" != "root" ]; then
  mkdir -p "${_REMOTE_USER_HOME}/.local/share/opencode"
  chown -R "${_REMOTE_USER}:${_REMOTE_USER}" "${_REMOTE_USER_HOME}/.local" 2>/dev/null || true
fi

opencode --version
