#!/bin/sh
set -eu
if command -v sshfs >/dev/null 2>&1; then
  exit 0
fi
if command -v apt-get >/dev/null 2>&1; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update
  apt-get install -y --no-install-recommends sshfs fuse3 || apt-get install -y --no-install-recommends sshfs fuse
  rm -rf /var/lib/apt/lists/*
elif command -v apk >/dev/null 2>&1; then
  apk add --no-cache sshfs fuse3 || apk add --no-cache sshfs fuse
elif command -v dnf >/dev/null 2>&1; then
  dnf install -y fuse-sshfs
  dnf clean all
elif command -v yum >/dev/null 2>&1; then
  yum install -y fuse-sshfs
  yum clean all
else
  echo "builtin:sshfs: no supported package manager (apt-get, apk, dnf, yum) found to install sshfs" >&2
  exit 1
fi
