# A custom actor image: needy's base image with one more tool, jq.
#
#   docker build -t needy-custom-jq examples/custom-image
#
# BASE is the local base image (`needy image build --linux-container`, or
# `--macos-apple-container`), or a released one,
# ghcr.io/needy-dev/needy-actor:v<version> (`needy image pull`), of the base
# ABI your needy speaks. A released
# ghcr.io/needy-dev/needy/actor-base:<version>-ubuntu-24.04 (pin its digest)
# works too: it is the same image without the shims, which the last step
# below adds. The container drivers run the image as it is; for
# the VM drivers, `needy image build --from needy-custom-jq` makes a VM
# image of it. Both pick it
# up from a profile's `actor.image: needy-custom-jq`.
ARG BASE=needy-actor:latest
FROM ${BASE}

# The base's shells are shims, which run their real programs unrouted
# while an image builds, so `RUN` works as on any Ubuntu image.
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends jq \
 && rm -rf /var/lib/apt/lists/*

# Last: keep the shells shimmed in place, in case a package added or upgraded
# one, so a container actor routes them by their absolute paths too. The
# other programs this image added are routed through the trampolines a
# container actor makes when it starts. The VM drivers do not need it (their
# kernel catches every exec), and `needy image build --from` puts the real
# programs back.
RUN ["/.needy/shim-image"]
