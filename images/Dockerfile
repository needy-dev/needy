# syntax=docker/dockerfile:1
# Builds everything an actor or executor needs.
#   docker build -f images/Dockerfile --target actor --output type=local,dest=OUT .
#   docker build -f images/Dockerfile --target executor -t needy-executor .
#   docker build -f images/Dockerfile --target actor-container -t needy-actor .
#   docker build -f images/Dockerfile --platform linux/arm64 --target actor-macos-vz --output type=local,dest=OUT .
#   docker build -f images/Dockerfile --target guest --output type=local,dest=OUT .
# Stages build for the platform they run on: `uname -m` picks the musl
# target, the kernel configuration and opencode's release. The Firecracker
# actor (`actor`) is x86_64 only; the `macos-vz` actor (`actor-macos-vz`,
# Virtualization.framework on Apple silicon) is arm64 only.
#
# The actor images are base images: they hold no needy of their own, only
# the boot stub and the shim stub (`images/stub/`). The daemon injects its
# own guest needy (the `guest` target's) when an actor starts. BASE_ABI is
# the base ABI the images record (`needy_core_actor::image::base::ABI`, which
# a test keeps equal); see
# docs/ai/references/architecture/2026-11-12-actor-images-design.md.
ARG KERNEL_VERSION=6.18.62
ARG FIRECRACKER_VERSION=v1.17.0
ARG OPENCODE_VERSION=v1.19.5
ARG BASE_ABI=1

# Static needy binary for guests and executors.
# NOTICE: the cache mount on /src/target is shared by every build on this
# builder, whatever source tree it came from: a cache mount's `id` defaults to
# its target path, and "your build should work with any contents of the cache
# directory as another build may overwrite the files",
# https://github.com/moby/buildkit/blob/v0.16.0/frontend/dockerfile/docs/reference.md?plain=1#L745-L766
# Cargo takes a workspace crate as fresh when no source file is newer than
# the dep-info file of its last build,
# https://github.com/rust-lang/cargo/blob/rust-1.85.0/src/cargo/core/compiler/fingerprint/mod.rs#L227-L237
# and COPY keeps the context's mtimes, so a tree unpacked from `git archive`
# (files dated at their commit) looked older than artifacts another tree had
# left in the cache, and cargo shipped those. Touching the sources first makes
# every workspace crate newer than anything in the cache, so it is rebuilt
# from this tree; registry crates, fingerprinted by version and checksum, stay
# cached. A cache `id` per tree would not do: two commits of one tree share
# it, and the older-looking files of the second meet the first's artifacts.
FROM rust:1 AS needy
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools && rm -rf /var/lib/apt/lists/* \
 && rustup target add "$(uname -m)-unknown-linux-musl"
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
    find /src -path /src/target -prune -o -type f -exec touch {} + \
 && target="$(uname -m)-unknown-linux-musl" \
 && cargo build --release --target "$target" -p needy-cli \
 && cp "target/$target/release/needy" /needy

# needy.exe for Windows executors.
FROM rust:1 AS needy-windows
RUN apt-get update && apt-get install -y --no-install-recommends gcc-mingw-w64-x86-64 && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-pc-windows-gnu
WORKDIR /src
COPY . .
# NOTICE: the sources are touched for the reason given in the `needy` stage.
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
    find /src -path /src/target -prune -o -type f -exec touch {} + \
 && cargo build --release --target x86_64-pc-windows-gnu -p needy-cli \
 && cp target/x86_64-pc-windows-gnu/release/needy.exe /needy.exe

FROM scratch AS windows
COPY --from=needy-windows /needy.exe /needy.exe

# The guest needy the daemon injects into actors, for this platform: what
# `make guest` writes as bin/needy-linux-<arch>.
FROM scratch AS guest
COPY --from=needy /needy /needy

# The e2e harness (`cargo xtask e2e`) as a static binary next to the static
# needy, for Linux hosts that run cells but build nothing (`--hosts`, role
# `run`): a build host exports both with the images.
# NOTICE: built on the `needy` stage with the same cache mounts, and the
# sources touched again for the reason given there.
FROM needy AS xtask
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
    find /src -path /src/target -prune -o -type f -exec touch {} + \
 && target="$(uname -m)-unknown-linux-musl" \
 && cargo build --release --locked --target "$target" -p xtask \
 && cp "target/$target/release/xtask" /xtask

FROM scratch AS e2e
COPY --from=needy /needy /needy
COPY --from=xtask /xtask /xtask

# The base image's `/.needy` (base ABI): the boot stub (`init`) and the shim
# stub (`proxy`), static musl C programs with no needy code, so an image
# works with every needy of its base ABI; an empty placeholder where the
# guest needy goes (`needy`); the directory a VM's boot stub mounts the
# guest disk at (`disk`); and the base ABI it was built for (`base-abi`).
# The build fails unless the stubs are static.
FROM ubuntu:24.04 AS stubs
ARG BASE_ABI
RUN apt-get update && apt-get install -y --no-install-recommends gcc libc6-dev musl-tools binutils \
 && rm -rf /var/lib/apt/lists/*
COPY images/stub/ /src/
RUN mkdir -p /out/.needy/disk \
 && musl-gcc -static -Os -s -Wall -Werror -o /out/.needy/init /src/boot.c \
 && musl-gcc -static -Os -s -Wall -Werror -o /out/.needy/proxy /src/shim.c \
 && for stub in /out/.needy/init /out/.needy/proxy; do \
      if readelf -lW "$stub" | grep -q INTERP || readelf -dW "$stub" | grep -q '(NEEDED)'; then \
        echo "$stub is not statically linked" >&2; exit 1; \
      fi; \
    done \
 && : > /out/.needy/needy && chmod 755 /out/.needy/needy \
 && echo "$BASE_ABI" > /out/.needy/base-abi \
 && echo "$BASE_ABI" > /out/base-abi

# Guest kernel: Firecracker CI config, needy fragment, exec routing and fake sensors.
# Source: https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/resources/guest_configs/microvm-kernel-ci-x86_64-6.18.config
# (v1.17.0; `images/kernel/firecracker-6.18.config` is an unchanged copy).
# On arm64 (the `macos-vz` actor) the base is Apple Containerization's arm64 config
# with `needy-arm64.config` on top (provenance in that file), and the result
# is the uncompressed `arch/arm64/boot/Image` Virtualization.framework boots.
# Reference: https://www.kernel.org/ (stable 6.18.y; the tarball URL below is
# pinned by KERNEL_VERSION).
FROM ubuntu:24.04 AS kernel
ARG KERNEL_VERSION
# The recipe's kernel identity (`crates/core/src/recipe/kernel.rs` says how
# each one becomes `uname` and `/proc/version`). Empty keeps the kernel's own.
ARG KERNELRELEASE=
ARG KBUILD_BUILD_VERSION=
ARG KBUILD_BUILD_USER=builder
ARG KBUILD_BUILD_HOST=needy
ARG KBUILD_BUILD_TIMESTAMP=
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential bc flex bison libelf-dev libssl-dev cpio python3 curl xz-utils ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /linux
RUN curl -fsSL https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-${KERNEL_VERSION}.tar.xz | tar xJ --strip-components=1
COPY images/kernel/firecracker-6.18.config images/kernel/containerization-arm64.config /tmp/
COPY images/kernel/needy.config images/kernel/needy-arm64.config /tmp/
COPY images/kernel/binfmt_needy.c fs/
COPY images/kernel/fake_coretemp.c drivers/hwmon/
RUN echo "obj-y += binfmt_needy.o" >> fs/Makefile \
 && echo "obj-y += fake_coretemp.o" >> drivers/hwmon/Makefile \
 && if [ "$(uname -m)" = aarch64 ]; then \
      cp /tmp/containerization-arm64.config .config \
      && scripts/kconfig/merge_config.sh -m .config /tmp/needy.config /tmp/needy-arm64.config >/dev/null; \
    else \
      cp /tmp/firecracker-6.18.config .config \
      && scripts/kconfig/merge_config.sh -m .config /tmp/needy.config >/dev/null; \
    fi \
 && scripts/config -d LOCALVERSION_AUTO --set-str LOCALVERSION "" \
 && make -s olddefconfig
# NOTICE: `KERNELRELEASE` works only on the make command line: the top
# Makefile assigns it, which hides the environment's value, and only a value
# whose origin is not that file replaces `scripts/setlocalversion`:
# https://github.com/gregkh/linux/blob/c3f63d70f236bb7685e6d11d1d9b5b351552e600/Makefile#L378-L381
# and #L1279-L1283 (v6.18.62). `KBUILD_BUILD_VERSION` from a recipe already
# holds the recipe's `SMP`/`PREEMPT_DYNAMIC` flags, so the configuration's
# flags (`smp-flag-y`, `preempt-flag-y`) are emptied on the command line, which
# overrides init/Makefile's assignments in the sub-make:
# https://github.com/gregkh/linux/blob/c3f63d70f236bb7685e6d11d1d9b5b351552e600/init/Makefile#L27-L38
# Both are internal names of this kernel version; recheck them when
# KERNEL_VERSION changes. Every module is built in (`obj-y`), so no module's
# vermagic has to match the new release.
RUN set --; \
    if [ -n "$KERNELRELEASE" ]; then set -- "$@" "KERNELRELEASE=$KERNELRELEASE"; fi; \
    if [ -n "$KBUILD_BUILD_VERSION" ]; then set -- "$@" smp-flag-y= preempt-flag-y=; fi; \
    KBUILD_BUILD_VERSION="$KBUILD_BUILD_VERSION" KBUILD_BUILD_USER="$KBUILD_BUILD_USER" \
    KBUILD_BUILD_HOST="$KBUILD_BUILD_HOST" KBUILD_BUILD_TIMESTAMP="$KBUILD_BUILD_TIMESTAMP" \
    make -s -j"$(nproc)" "$@" "$(if [ "$(uname -m)" = aarch64 ]; then echo Image; else echo vmlinux; fi)"

# Firecracker with needy's CPUID, SMBIOS and PCI identity patch, linked
# statically against musl as upstream releases are, so it runs on any Linux
# host whatever its glibc (Debian 12 has 2.36; a glibc build needed 2.38).
# Source: https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/tools/release.sh#L64
# and #L162-L164 (v1.17.0: musl is the default libc and the only one releases use).
# Reference: https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/docs/getting-started.md#building-firecracker
# NOTICE: the `seccompiler` crate links `libseccomp`
# (https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/src/seccompiler/build.rs#L4-L7),
# so a static musl `libseccomp` goes into `/usr/local/lib`, and `musl-gcc`
# gets the kernel headers, as upstream's development container does:
# https://github.com/firecracker-microvm/firecracker/blob/95f868c8e345b1cc8faccd1a3c910b4989dc3f58/tools/devctr/Dockerfile#L41-L45
# and #L73-L90. The SHA-256 is the release's `libseccomp-2.6.1.tar.gz.SHA256SUM`:
# https://github.com/seccomp/libseccomp/releases/tag/v2.6.1
FROM rust:1 AS firecracker
ARG FIRECRACKER_VERSION
ARG LIBSECCOMP_VERSION=2.6.1
ARG LIBSECCOMP_SHA256=501f66c667225d53791b97e1d7cf85ab764c297d04881f60f38f451c4b0ee1be
RUN apt-get update && apt-get install -y --no-install-recommends libclang-dev cmake patch musl-tools gperf \
 && rm -rf /var/lib/apt/lists/* \
 && cd /usr/include/x86_64-linux-musl \
 && ln -s ../x86_64-linux-gnu/asm asm && ln -s ../linux linux && ln -s ../asm-generic asm-generic
RUN curl -fsSL -o /tmp/libseccomp.tar.gz \
      https://github.com/seccomp/libseccomp/releases/download/v${LIBSECCOMP_VERSION}/libseccomp-${LIBSECCOMP_VERSION}.tar.gz \
 && echo "${LIBSECCOMP_SHA256}  /tmp/libseccomp.tar.gz" | sha256sum -c - \
 && mkdir /tmp/libseccomp && tar xzf /tmp/libseccomp.tar.gz -C /tmp/libseccomp --strip-components=1 \
 && cd /tmp/libseccomp \
 && CC="musl-gcc -static" ./configure --prefix=/usr/local --enable-static=yes --enable-shared=false \
 && make -s -j"$(nproc)" && make -s install \
 && rm -rf /tmp/libseccomp /tmp/libseccomp.tar.gz
WORKDIR /fc
RUN curl -fsSL https://github.com/firecracker-microvm/firecracker/archive/refs/tags/${FIRECRACKER_VERSION}.tar.gz | tar xz --strip-components=1
COPY images/firecracker/needy.patch /tmp/
# The build fails unless the result is static: no program interpreter and no
# shared libraries.
RUN patch -p1 < /tmp/needy.patch \
 && cargo build --release -p firecracker --target x86_64-unknown-linux-musl \
 && cp build/cargo_target/x86_64-unknown-linux-musl/release/firecracker /firecracker \
 && if readelf -lW /firecracker | grep -q INTERP || readelf -dW /firecracker | grep -q '(NEEDED)'; then \
      echo "firecracker is not statically linked:" >&2; readelf -dW /firecracker | grep '(NEEDED)' >&2; exit 1; \
    fi

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
 && arch="$(case "$(uname -m)" in aarch64) echo arm64;; *) echo x64;; esac)" \
 && curl -fsSL "https://github.com/sst/opencode/releases/download/${OPENCODE_VERSION}/opencode-linux-${arch}.tar.gz" \
    | tar xz -C /usr/local/bin opencode \
 && mkdir -p /home/agent/workspace && chown agent:agent /home/agent/workspace
COPY --from=stubs /out/.needy/ /.needy/

FROM ubuntu:24.04 AS actor-image
RUN apt-get update && apt-get install -y --no-install-recommends e2fsprogs && rm -rf /var/lib/apt/lists/*
COPY --from=actor-root / /r
# NOTICE: the image is sized to its content: the blocks `du` counts plus 10%
# and 64 MiB for ext4's journal, inode tables, bitmaps, directories and extent
# trees, and the inodes in use plus 10%. There is no room to grow on purpose:
# Firecracker attaches the rootfs read-only (`is_read_only` in
# `crates/core-actor/src/driver/linux_firecracker/config.rs`) and the guest's writable places are
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
COPY --from=stubs /out/base-abi /base-abi

# The `macos-vz` actor's files, built with `--platform linux/arm64`: the
# uncompressed kernel Virtualization.framework boots and the actor rootfs.
# The VM process is vfkit, installed on the Mac, not built here.
# NOTICE: `initrd` is an empty initramfs (a cpio archive with only its
# trailer). vfkit always hands its Linux boot loader an initrd path, and an
# empty one fails (`invalid initial RAM disk path`, vfkit 0.6.4,
# https://github.com/crc-org/vfkit/blob/v0.6.4/pkg/vf/bootloader.go#L59-L63).
# Without `/init` in it the kernel mounts `root=` and starts `init=` as it
# does without an initrd: https://github.com/torvalds/linux/blob/v6.18/init/main.c#L1604-L1611
FROM ubuntu:24.04 AS macos-vz-initrd
RUN apt-get update && apt-get install -y --no-install-recommends cpio && rm -rf /var/lib/apt/lists/* \
 && cpio -o -H newc < /dev/null > /initrd

FROM scratch AS actor-macos-vz
COPY --from=kernel /linux/arch/arm64/boot/Image /Image
COPY --from=macos-vz-initrd /initrd /initrd
COPY --from=actor-image /rootfs.ext4 /rootfs.ext4
COPY --from=stubs /out/base-abi /base-abi

# Container actor userland: the actor-root's programs without what only a VM
# needs: no systemd (a container's `systemd-detect-virt` would name the
# container anyway). sshfs is there for Apple's `container`, whose actor
# mounts the workspace inside (a Mac has no FUSE for the host side); a Docker
# container actor's runtime mounts it on the host. Its last step turns every
# program on PATH into a shim
# (images/container/shim.sh).
# It builds for x86_64 (Docker on Linux) and arm64 (Apple's `container` on
# Apple silicon, `--platform linux/arm64`).
FROM ubuntu:24.04 AS container-root
ARG OPENCODE_VERSION
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      bash git ripgrep ca-certificates curl sshfs fuse3 iproute2 procps less file xz-utils \
      pciutils dmidecode lm-sensors util-linux \
 && rm -rf /var/lib/apt/lists/* \
 && userdel -r ubuntu 2>/dev/null; useradd -m -u 1000 -s /bin/bash agent \
 && arch="$(case "$(uname -m)" in aarch64) echo arm64;; *) echo x64;; esac)" \
 && curl -fsSL "https://github.com/sst/opencode/releases/download/${OPENCODE_VERSION}/opencode-linux-${arch}.tar.gz" \
    | tar xz -C /usr/local/bin opencode \
 && mkdir -p /.needy/channels
COPY --from=stubs /out/.needy/ /.needy/
# The script stays in the image as `/.needy/shim-image`: an image `FROM`
# this one runs it as its last step, so the programs it adds are shims too
# (`examples/custom-image/Dockerfile`). Without arguments it shims Docker's
# default container PATH.
COPY --chmod=755 images/container/shim.sh /.needy/shim-image
RUN /.needy/shim-image

# The container actor image, `needy-actor`: one layer, so the copies the
# shim step made of programs from earlier layers are not stored twice. The
# label records its base ABI, which the container drivers check.
FROM scratch AS actor-container
ARG BASE_ABI
COPY --from=container-root / /
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
LABEL needy.base-abi=$BASE_ABI
ENTRYPOINT ["/.needy/init"]

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
