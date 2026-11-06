ARG RUST_VERSION=1.97
ARG BINARY_STAGE=build
ARG NODE_IMAGE=node:24-bookworm-slim

FROM rust:${RUST_VERSION}-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release --package needy \
    && mkdir /out && cp target/release/needy /out/needy

# Optional: put a Linux binary in .needy/image/needy, matching image architecture.
FROM scratch AS prebuilt
COPY .needy/image/needy /out/needy

FROM ${BINARY_STAGE} AS binary

FROM ${NODE_IMAGE} AS runtime
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git openssh-client ripgrep procps \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /workspace /home/needy \
    && chown 1000:1000 /workspace /home/needy
COPY --from=binary /out/needy /opt/needy/needy
RUN /opt/needy/needy --version
ENV HOME=/home/needy
WORKDIR /workspace
USER 1000:1000
ENTRYPOINT ["/opt/needy/needy", "runner"]


