# A custom actor image: needy's base image with one more tool, jq.
#
# For the container drivers, on the container base (shims included):
#
#   docker build -t needy-custom-jq examples/custom-image
#
# BASE is the local container base (`needy image build --linux-container`,
# or `--macos-apple-container`), or a released one,
# ghcr.io/needy-dev/needy-actor:v<version> (`needy image pull`), of the base
# ABI your needy speaks. The container drivers run the image as it is, from
# a profile's `actor.image: needy-custom-jq`.
#
# For the VM drivers, on actor-base, without the shim step, pushed to a
# registry: a VM actor imports the image's layers above actor-base from it.
# BASE is the actor-base your base VM image records (`needy image ls`; the
# release's ghcr.io/needy-dev/needy/actor-base@sha256:..., or
# needy-actor-base:latest after `needy image build`):
#
#   docker build --build-arg BASE=ghcr.io/needy-dev/needy/actor-base@sha256:<digest> \
#     --build-arg SHIM=0 -t registry.example.com/needy-custom-jq:1 examples/custom-image
#   docker push registry.example.com/needy-custom-jq:1
#
# and name it in `actor.image: registry.example.com/needy-custom-jq:1`
# (`http://` before a plain-HTTP registry such as `localhost:5000`).
ARG BASE=needy-actor:latest
FROM ${BASE}
ARG SHIM=1

# On the container base the shells are shims, which run their real programs
# unrouted while an image builds, so `RUN` works as on any Ubuntu image.
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends jq \
 && rm -rf /var/lib/apt/lists/*

# Last, for the container drivers: keep the shells shimmed in place, in case
# a package added or upgraded one, so a container actor routes them by their
# absolute paths too. The other programs this image added are routed through
# the trampolines a container actor makes when it starts. The VM drivers do
# not need it (their kernel catches every exec) and refuse an image that ran
# it (it writes under /.needy), so `SHIM=0` leaves it out.
RUN if [ "$SHIM" = 1 ]; then /.needy/shim-image; fi
