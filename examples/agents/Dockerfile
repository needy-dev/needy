# The agents image: needy's base image plus Node.js, pi, Claude Code, Codex
# and herdr (opencode is in the base already), for the profiles next to this
# file. Build it for this machine's architecture (linux/amd64 on a Linux
# host, linux/arm64 on Apple silicon):
#
#   docker build -t needy-agents examples/agents
#
# BASE is the local base image (`needy image build --linux-container`, or
# `--macos-apple-container`), or a released one of the base ABI your needy
# speaks (`needy image pull` fetches the one of your needy's release):
#
#   docker build --build-arg BASE=ghcr.io/needy-dev/needy-actor:v<version> -t needy-agents examples/agents
#
# The container drivers run the image as it is; the VM drivers boot the VM
# image `needy image build --from needy-agents` makes of it (see README.md).
#
# Every download below is pinned to one release and checked against a
# SHA-256 (or, for npm, SHA-512) taken from that release on 2026-11-13. To
# update one, change its version and both checksums together, from the
# source its comment names.
ARG BASE=needy-actor:latest
FROM ${BASE}

# BuildKit sets TARGETARCH to amd64 or arm64 from the build's platform.
ARG TARGETARCH

# NOTICE: Node.js's official Linux builds link libatomic.so.1 (the arm64 one
# fails to start without it: `libatomic.so.1: cannot open shared object
# file`), which Ubuntu's minimal image does not have. pi's file search uses
# fd, which it otherwise downloads from GitHub at its first start (`fd not
# found. Downloading...`); it accepts Ubuntu's `fdfind` (`systemBinaryNames`
# in pi 1.5.2's dist/bundle). jq is for reading agent configs in the actor.
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends libatomic1 fd-find jq \
 && rm -rf /var/lib/apt/lists/*

# Node.js, the runtime pi needs: the current LTS line (26, "Lithium").
# Source: https://nodejs.org/dist/v26.13.1/SHASUMS256.txt (the release's
# checksums; the file is signed by the release key, SHASUMS256.txt.sig)
# Reference: https://nodejs.org/en/about/previous-releases
ARG NODE_VERSION=26.13.1
ARG NODE_SHA256_AMD64=5c063e46bab0fbc1c17cc445d7ff129cbdcd0471be6ed83b6ab864f8143bc864
ARG NODE_SHA256_ARM64=89e1853138db51e8b82d3e98ba9c0d3f2956a28e5e55ea1b7e12d25aae92d5dd
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) arch=x64; sum=$NODE_SHA256_AMD64 ;; \
      arm64) arch=arm64; sum=$NODE_SHA256_ARM64 ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/node.tar.xz "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${arch}.tar.xz"; \
    echo "$sum  /tmp/node.tar.xz" | sha256sum -c -; \
    tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 --no-same-owner \
      --exclude='*/CHANGELOG.md' --exclude='*/README.md' --exclude='*/LICENSE' --exclude='*/share/doc'; \
    rm /tmp/node.tar.xz; \
    node --version

# pi, the coding agent. Its npm package moved from
# @mariozechner/pi-coding-agent (deprecated at 0.73.1: "please use
# @earendil-works/pi-coding-agent instead") to @earendil-works.
# Source: https://registry.npmjs.org/@earendil-works/pi-coding-agent/1.5.2
# (`dist.integrity`, sha512-mUWI04rq..., written here as hex), release
# https://github.com/earendil-works/pi/releases/tag/v1.5.2
# Reference: https://github.com/earendil-works/pi/blob/v1.5.2/packages/coding-agent/docs/containerization.md
# (`npm install -g --ignore-scripts`, as pi's own container recipe does)
# NOTICE: the checksum pins pi's own package; npm resolves its dependencies
# from the registry when the image builds. pi pins its direct dependencies
# to exact versions, but theirs follow semver ranges.
ARG PI_VERSION=1.5.2
ARG PI_SHA512=994588d38aea17555dfe6d6af8fb355de5ac4d7115bd1527b2645af372370f3b9ad13c796d84bdde299eaad21fad77fb3e491a47a111cfe02e4212099689b9c8
RUN set -eu; \
    curl -fsSL -o /tmp/pi.tgz "https://registry.npmjs.org/@earendil-works/pi-coding-agent/-/pi-coding-agent-${PI_VERSION}.tgz"; \
    echo "$PI_SHA512  /tmp/pi.tgz" | sha512sum -c -; \
    npm install -g --ignore-scripts --no-audit --no-fund /tmp/pi.tgz; \
    rm -rf /tmp/pi.tgz /root/.npm; \
    pi --version

# Claude Code, the native build (a single binary that needs no Node.js), on
# the stable channel. It is what the official installer
# (https://claude.ai/install.sh) downloads, without the installer's
# `claude install` step, which writes into the building user's home.
# Source: https://downloads.claude.ai/claude-code-releases/2.1.311/manifest.json
# (`platforms.linux-x64.checksum`, `platforms.linux-arm64.checksum`; build
# commit e7d3b8365a0ffbe2f5dd032feb28e9fd9c7de025, 2026-10-26)
# Reference: https://code.claude.com/docs/en/setup
ARG CLAUDE_VERSION=2.1.311
ARG CLAUDE_SHA256_AMD64=86dc1fcc69498159eaf15f446613e847010f9036bfbbdffd4c49350be8489ada
ARG CLAUDE_SHA256_ARM64=2e2496c9a8065df93acb0fca2d341f59a6b8777649b2e025ab8c9bcd5eef61f3
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) platform=linux-x64; sum=$CLAUDE_SHA256_AMD64 ;; \
      arm64) platform=linux-arm64; sum=$CLAUDE_SHA256_ARM64 ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /usr/local/bin/claude "https://downloads.claude.ai/claude-code-releases/${CLAUDE_VERSION}/${platform}/claude"; \
    echo "$sum  /usr/local/bin/claude" | sha256sum -c -; \
    chmod 0755 /usr/local/bin/claude; \
    DISABLE_AUTOUPDATER=1 claude --version

# Codex, the static musl build, as the release's package: the codex binary
# with its helpers (codex-code-mode-host, codex-exec-server, rg, bwrap) and
# codex-package.json, the layout Codex's standalone installer
# (https://chatgpt.com/codex/install.sh, `install_package_release`) unpacks.
# NOTICE: not the bare `codex-<target>.tar.gz` binary the README lists: it
# starts as a driving program, but in a herdr pane it stops with "this CLI
# has no complete local package; install a packaged Codex CLI or use the
# standalone installer" (seen with 0.167.2 in a macos-vz actor).
# Source: https://github.com/openai/codex/releases/tag/rust-v0.167.2
# (`codex-package-<target>.tar.gz`; each asset's `digest` in the GitHub
# release API, as in the release's codex-package_SHA256SUMS)
# Reference: https://github.com/openai/codex/blob/rust-v0.167.2/README.md
ARG CODEX_VERSION=0.167.2
ARG CODEX_SHA256_AMD64=3d77be1748d35a4bb0e7f8752cde3b11890c63338e8c599f01eafa4a59e17b3d
ARG CODEX_SHA256_ARM64=56b08c1214e062a5b61684bacb5033c4372a01062a47c87c49a32be83af70695
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl; sum=$CODEX_SHA256_AMD64 ;; \
      arm64) target=aarch64-unknown-linux-musl; sum=$CODEX_SHA256_ARM64 ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/codex.tar.gz "https://github.com/openai/codex/releases/download/rust-v${CODEX_VERSION}/codex-package-${target}.tar.gz"; \
    echo "$sum  /tmp/codex.tar.gz" | sha256sum -c -; \
    mkdir -p /opt/codex; \
    tar -xzf /tmp/codex.tar.gz -C /opt/codex --no-same-owner; \
    rm -f /tmp/codex.tar.gz; \
    ln -s /opt/codex/bin/codex /usr/local/bin/codex; \
    codex --version

# herdr, which runs agents in panes. Its repository moved from
# ogulcancelik/herdr to herdrdev/herdr.
# Source: https://github.com/herdrdev/herdr/releases/tag/v0.9.5 (each
# asset's `digest`; the same values as https://herdr.dev/latest.json, which
# herdr's installer reads)
# Reference: https://github.com/herdrdev/herdr/blob/v0.9.5/README.md
ARG HERDR_VERSION=0.9.5
ARG HERDR_SHA256_AMD64=14d9e8bc8b4a9e0118d77c32266f76d461f09b40342eb23752960bd89f59929d
ARG HERDR_SHA256_ARM64=17d7536f2da50859523680ef279dffcac8e4a235f4e46fe8da6c2ec44141897c
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) arch=x86_64; sum=$HERDR_SHA256_AMD64 ;; \
      arm64) arch=aarch64; sum=$HERDR_SHA256_ARM64 ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /usr/local/bin/herdr "https://github.com/herdrdev/herdr/releases/download/v${HERDR_VERSION}/herdr-linux-${arch}"; \
    echo "$sum  /usr/local/bin/herdr" | sha256sum -c -; \
    chmod 0755 /usr/local/bin/herdr; \
    herdr --version

# Last: make the programs this image added shims too, so a container
# actor's rules can route them. The VM drivers do not need it (their kernel
# catches every exec), and `needy image build --from` puts the real programs back.
RUN ["/.needy/shim-image"]
