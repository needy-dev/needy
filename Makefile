CARGO ?= cargo
BUF ?= buf
DOCKER ?= docker
# The guest needy's architecture: this machine's, as Rust names it.
GUEST_ARCH ?= $(shell uname -m | sed -e 's/^arm64$$/aarch64/' -e 's/^amd64$$/x86_64/')

.PHONY: build release guest test check proto

build:
	$(CARGO) build --locked --bin needy
	mkdir -p bin
	install -m 0755 target/debug/needy bin/needy

release:
	$(CARGO) build --locked --release --bin needy
	mkdir -p bin
	install -m 0755 target/release/needy bin/needy

# The guest needy the daemon injects into actors: a static musl build for
# this machine's architecture, through Docker (images/Dockerfile's `guest`
# target) on Linux and macOS alike. It lands next to `make build`'s
# bin/needy, where the daemon looks for it.
guest:
	$(DOCKER) build -f images/Dockerfile --target guest --output type=local,dest=bin/.guest .
	install -m 0755 bin/.guest/needy bin/needy-linux-$(GUEST_ARCH)
	rm -rf bin/.guest

test:
	$(CARGO) test --locked --workspace

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --locked --workspace --all-targets -- -D warnings

proto:
	cd proto && $(BUF) lint && $(BUF) format -w && $(BUF) generate
