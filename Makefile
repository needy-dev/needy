CARGO ?= cargo
DOCKER ?= docker
BUF ?= buf
BINARY_STAGE ?= build
IMAGE ?= needy-runtime:rust
TEST_IMAGE ?= needy-rust-test:dev

.PHONY: build release test check host-test docker-test image test-image proto

build:
	$(CARGO) build --locked --bin needy
	mkdir -p bin
	install -m 0755 target/debug/needy bin/needy

release:
	$(CARGO) build --locked --release --bin needy
	mkdir -p bin
	install -m 0755 target/release/needy bin/needy

test:
	$(CARGO) test --locked --workspace

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --locked --workspace --all-targets -- -D warnings

host-test: build
	python3 scripts/test-rust-host.py bin/needy

docker-test: build
	python3 scripts/test-rust-docker.py bin/needy

image:
	$(DOCKER) build --build-arg BINARY_STAGE=$(BINARY_STAGE) -t $(IMAGE) -f images/Dockerfile .

test-image:
	$(DOCKER) build --build-arg BINARY_STAGE=$(BINARY_STAGE) -t $(TEST_IMAGE) -f images/Dockerfile .

proto:
	cd proto && $(BUF) lint && $(BUF) format -w && $(BUF) generate
