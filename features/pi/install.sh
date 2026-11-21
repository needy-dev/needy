#!/bin/sh
# pi, the coding agent (@earendil-works/pi-coding-agent), with Node.js LTS
# installed when absent.
#
# Node.js LTS (26.13.1):
# Source: https://nodejs.org/dist/v26.13.1/SHASUMS256.txt (retrieved 2026-11-13)
# Reference: https://nodejs.org/en/about/previous-releases
#
# pi (1.5.2):
# Source: https://registry.npmjs.org/@earendil-works/pi-coding-agent/1.5.2
# (`dist.integrity`, sha512, retrieved 2026-11-13), release
# https://github.com/earendil-works/pi/releases/tag/v1.5.2
# Reference: https://github.com/earendil-works/pi/blob/v1.5.2/packages/coding-agent/docs/containerization.md
set -eu

VERSION="${VERSION:-1.5.2}"
PINNED_VERSION="1.5.2"
PI_SHA512="994588d38aea17555dfe6d6af8fb355de5ac4d7115bd1527b2645af372370f3b9ad13c796d84bdde299eaad21fad77fb3e491a47a111cfe02e4212099689b9c8"

NODE_VERSION="26.13.1"
NODE_SHA256_AMD64="5c063e46bab0fbc1c17cc445d7ff129cbdcd0471be6ed83b6ab864f8143bc864"
NODE_SHA256_ARM64="89e1853138db51e8b82d3e98ba9c0d3f2956a28e5e55ea1b7e12d25aae92d5dd"

if command -v apt-get >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends curl ca-certificates xz-utils libatomic1 fd-find jq
  rm -rf /var/lib/apt/lists/*
elif command -v apk >/dev/null 2>&1; then
  apk add --no-cache curl ca-certificates xz fd jq
fi

if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
  case "$(uname -m)" in
    x86_64|amd64) arch="x64"; sum="$NODE_SHA256_AMD64" ;;
    aarch64|arm64) arch="arm64"; sum="$NODE_SHA256_ARM64" ;;
    *) echo "pi feature: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
  esac
  curl -fsSL -o /tmp/node.tar.xz "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${arch}.tar.xz"
  echo "${sum}  /tmp/node.tar.xz" | sha256sum -c -
  tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 --no-same-owner \
    --exclude='*/CHANGELOG.md' --exclude='*/README.md' --exclude='*/LICENSE' --exclude='*/share/doc'
  rm -f /tmp/node.tar.xz
fi

curl -fsSL -o /tmp/pi.tgz "https://registry.npmjs.org/@earendil-works/pi-coding-agent/-/pi-coding-agent-${VERSION}.tgz"
if [ "$VERSION" = "$PINNED_VERSION" ]; then
  echo "${PI_SHA512}  /tmp/pi.tgz" | sha512sum -c -
fi
npm install -g --ignore-scripts --no-audit --no-fund /tmp/pi.tgz
rm -rf /tmp/pi.tgz /root/.npm

if [ -n "${_REMOTE_USER:-}" ] && [ -n "${_REMOTE_USER_HOME:-}" ] && [ "$_REMOTE_USER" != "root" ]; then
  mkdir -p "${_REMOTE_USER_HOME}/.pi/agent"
  chown -R "${_REMOTE_USER}:${_REMOTE_USER}" "${_REMOTE_USER_HOME}/.pi" 2>/dev/null || true
fi

pi --version
