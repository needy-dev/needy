# A custom actor image: needy's base image with one more tool, jq.
#
#   docker build -t needy-custom-jq examples/custom-image
#
# BASE is the local base image (`needy build --linux-container`, or
# `--macos-apple-container`), or a released one,
# ghcr.io/needy-dev/needy-actor:<tag>, of the base ABI your needy speaks.
# The container drivers run the image as it is; for the VM drivers,
# `needy build --from needy-custom-jq` makes a VM image of it. Both pick it
# up from a profile's `actor.image: needy-custom-jq`.
ARG BASE=needy-actor:latest
FROM ${BASE}

# The base's programs are shims, which run their real programs unrouted
# while an image builds, so `RUN` works as on any Ubuntu image.
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends jq \
 && rm -rf /var/lib/apt/lists/*

# Last: make the programs this image added shims too, so a container
# actor's rules can route them. The VM drivers do not need it (their kernel
# catches every exec), and `needy build --from` puts the real programs back.
RUN ["/.needy/shim-image"]
