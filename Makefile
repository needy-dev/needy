CARGO ?= cargo
BUF ?= buf

.PHONY: build release test check proto

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

proto:
	cd proto && $(BUF) lint && $(BUF) format -w && $(BUF) generate
