VERSION := $(shell sed -n 's/^version = "\(.*\)"/v\1/p' rust/Cargo.toml | head -n1)

.PHONY: build install test bench release clean

## build: build the binary at ./chrono for your platform
build:
	cd rust && cargo build --release --locked -q -p chrono-cli
	cp rust/target/release/chrono ./chrono

## install: install chrono into ~/.cargo/bin
install:
	cargo install --path rust/crates/chrono-cli --locked

## test: unit tests + deterministic regression bench
test:
	cd rust && cargo test --locked -q && cargo build --locked -q -p chrono-cli
	python3 -I bench/agentes/regresion/oro_chrono.py $(CURDIR)/rust/target/debug/chrono

## bench: scale bench at 1M events (compares with the committed baseline)
bench: build
	python3 -I bench/escala/medir.py ./chrono --out bench/escala/ultima.json --base bench/escala/base-0.2.7.json

## release: compressed binary + checksum in ./dist for this platform (CI builds all four)
release: clean build
	@os=$$(uname -s | tr '[:upper:]' '[:lower:]'); arch=$$(uname -m); \
	case $$arch in x86_64) arch=amd64;; aarch64) arch=arm64;; esac; \
	name=chrono-$(VERSION)-$$os-$$arch; mkdir -p dist/$$name; \
	cp chrono LICENSE README.md README.es.md dist/$$name/; \
	tar -C dist -czf dist/$$name.tar.gz $$name; rm -rf dist/$$name; \
	cd dist && shasum -a 256 *.tar.gz > SHA256SUMS && ls -1

## clean: remove build artifacts
clean:
	rm -rf dist chrono result
