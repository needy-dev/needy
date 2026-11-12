# A VM root file system made from an OCI image `FROM` needy's container
# base image, for `needy build --from` (crates/cli/src/commands/build/from.rs).
# FROM is the image; the build runs for the VM driver's platform. The result
# is the `rootfs` target's rootfs.ext4; the kernel and the stubs come from the
# base (the kernel from the base VM image, the stubs through the image's own
# `FROM` the base). See
# docs/ai/references/architecture/2026-11-12-actor-images-design.md, step 2.
ARG FROM

FROM ${FROM} AS root

FROM ubuntu:24.04 AS image
RUN apt-get update && apt-get install -y --no-install-recommends e2fsprogs && rm -rf /var/lib/apt/lists/*
COPY --from=root / /r
# A VM's kernel catches every exec (`binfmt_needy`), so the container
# image's shims are put back as the real programs they stand for
# (`/.needy/real/<path>`, images/container/shim.sh): each is removed before
# its real program goes in its place, since a shim is a hard link to the
# proxy and writing through it would change them all.
# NOTICE: the image is sized to its content as the base VM image is (see
# `actor-image` in images/Dockerfile): the blocks `du` counts plus 10% and
# 64 MiB for ext4's own structures, the inodes in use plus 10%. The VM
# drivers attach it read-only.
RUN set -eu; \
    if [ -d /r/.needy/real ]; then \
      cp -a --remove-destination /r/.needy/real/. /r/; \
      rm -rf /r/.needy/real; \
    fi; \
    rm -f /r/.dockerenv; \
    kib=$(du -sk /r | cut -f1); inodes=$(find /r | wc -l); \
    mke2fs -q -t ext4 -N $((inodes + inodes / 10)) -d /r /rootfs.ext4 "$((kib + kib / 10 + 64 * 1024))k"; \
    e2fsck -fn /rootfs.ext4

FROM scratch AS rootfs
COPY --from=image /rootfs.ext4 /rootfs.ext4
