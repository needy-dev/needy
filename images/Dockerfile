# syntax=docker/dockerfile:1
# Builds everything an actor or executor needs.
#   docker build -f images/Dockerfile --target actor --output type=local,dest=OUT .
#   docker build -f images/Dockerfile --target executor -t needy-executor .
ARG KERNEL_VERSION=6.18.62
ARG FIRECRACKER_VERSION=v1.17.0
ARG OPENCODE_VERSION=v1.19.5

# Static needy binary for guests and executors.
FROM rust:1 AS needy
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-unknown-linux-musl
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
    cargo build --release --target x86_64-unknown-linux-musl -p needy-cli \
 && cp target/x86_64-unknown-linux-musl/release/needy /needy

# needy.exe for Windows executors.
FROM rust:1 AS needy-windows
RUN apt-get update && apt-get install -y --no-install-recommends gcc-mingw-w64-x86-64 && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-pc-windows-gnu
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
    cargo build --release --target x86_64-pc-windows-gnu -p needy-cli \
 && cp target/x86_64-pc-windows-gnu/release/needy.exe /needy.exe

FROM scratch AS windows
COPY --from=needy-windows /needy.exe /needy.exe

# Guest kernel: Firecracker CI config, needy fragment, exec routing and fake sensors.
FROM ubuntu:24.04 AS kernel
ARG KERNEL_VERSION
ARG KBUILD_BUILD_USER=builder
ARG KBUILD_BUILD_HOST=needy
ARG KBUILD_BUILD_TIMESTAMP=
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential bc flex bison libelf-dev libssl-dev cpio python3 curl xz-utils ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /linux
RUN curl -fsSL https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${KERNEL_VERSION}.tar.xz | tar xJ --strip-components=1
COPY images/kernel/firecracker-6.18.config .config
COPY images/kernel/needy.config /tmp/needy.config
COPY images/kernel/binfmt_needy.c fs/
COPY images/kernel/fake_coretemp.c drivers/hwmon/
RUN echo "obj-y += binfmt_needy.o" >> fs/Makefile \
 && echo "obj-y += fake_coretemp.o" >> drivers/hwmon/Makefile \
 && scripts/kconfig/merge_config.sh -m .config /tmp/needy.config >/dev/null \
 && scripts/config -d LOCALVERSION_AUTO --set-str LOCALVERSION "" \
 && make -s olddefconfig \
 && KBUILD_BUILD_USER=$KBUILD_BUILD_USER KBUILD_BUILD_HOST=$KBUILD_BUILD_HOST \
    KBUILD_BUILD_TIMESTAMP="$KBUILD_BUILD_TIMESTAMP" make -s -j"$(nproc)" vmlinux

# Firecracker with needy's CPUID, SMBIOS and PCI identity patch.
FROM rust:1 AS firecracker
ARG FIRECRACKER_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends libclang-dev libseccomp-dev cmake patch \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /fc
RUN curl -fsSL https://github.com/firecracker-microvm/firecracker/archive/refs/tags/${FIRECRACKER_VERSION}.tar.gz | tar xz --strip-components=1
COPY images/firecracker/needy.patch /tmp/
RUN patch -p1 < /tmp/needy.patch \
 && cargo build --release -p firecracker --target x86_64-unknown-linux-gnu \
 && cp build/cargo_target/x86_64-unknown-linux-gnu/release/firecracker /firecracker

# Actor userland: the driving program's world. `systemd` is not run (needy's
# init is PID 1); it brings `systemd-detect-virt`, which a cloud VM has and
# which reads the recipe's DMI identity.
FROM ubuntu:24.04 AS actor-root
ARG OPENCODE_VERSION
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      bash git ripgrep ca-certificates curl sshfs fuse3 iproute2 procps less file xz-utils \
      pciutils dmidecode lm-sensors util-linux systemd \
 && rm -rf /var/lib/apt/lists/* /run/systemd/container \
 && userdel -r ubuntu 2>/dev/null; useradd -m -u 1000 -s /bin/bash agent \
 && curl -fsSL https://github.com/sst/opencode/releases/download/${OPENCODE_VERSION}/opencode-linux-x64.tar.gz \
    | tar xz -C /usr/local/bin opencode \
 && mkdir -p /.needy /home/agent/workspace && chown agent:agent /home/agent/workspace
COPY --from=needy /needy /.needy/needy
RUN ln /.needy/needy /.needy/init && ln /.needy/needy /.needy/proxy

FROM ubuntu:24.04 AS actor-image
RUN apt-get update && apt-get install -y --no-install-recommends e2fsprogs && rm -rf /var/lib/apt/lists/*
COPY --from=actor-root / /r
# NOTICE: the image is sized to its content: the blocks `du` counts plus 10%
# and 64 MiB for ext4's journal, inode tables, bitmaps, directories and extent
# trees, and the inodes in use plus 10%. There is no room to grow on purpose:
# Firecracker attaches the rootfs read-only (`is_read_only` in
# `crates/core-actor/src/host/vm.rs`) and the guest's writable places are
# tmpfs. If the rootfs ever becomes writable, give it headroom here. A margin
# too small fails the build in `mke2fs -d` ("Could not allocate block");
# `e2fsck -fn` checks the result.
# TODO(rootfs): an EROFS image with compression, and the driving program
# (opencode, about 200 MB) outside the base rootfs, would shrink it further;
# neither is approved yet.
RUN set -eu; \
    kib=$(du -sk /r | cut -f1); inodes=$(find /r | wc -l); \
    mke2fs -q -t ext4 -N $((inodes + inodes / 10)) -d /r /rootfs.ext4 "$((kib + kib / 10 + 64 * 1024))k"; \
    e2fsck -fn /rootfs.ext4

FROM scratch AS actor
COPY --from=kernel /linux/vmlinux /vmlinux
COPY --from=actor-image /rootfs.ext4 /rootfs.ext4
COPY --from=firecracker /firecracker /firecracker
COPY --from=needy /needy /needy

# Executor: a container where routed commands run.
FROM ubuntu:24.04 AS executor
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      bash git ripgrep ca-certificates curl openssh-sftp-server openssh-client python3 python3-pip \
      build-essential nodejs npm procps less file iproute2 \
 && rm -rf /var/lib/apt/lists/* \
 && userdel -r ubuntu 2>/dev/null; useradd -m -u 1000 -s /bin/bash agent \
 && mkdir -p /home/agent/workspace /run/needy && chown agent:agent /home/agent/workspace /run/needy
COPY --from=needy /needy /usr/local/bin/needy
USER agent
WORKDIR /home/agent/workspace
CMD ["needy", "daemon", "--mode", "executor", "--listen", "unix:/run/needy/executor.sock", "--name", "executor"]
