VERSION ?= v0.1.0
LDFLAGS := -s -w -X main.version=$(VERSION)
PLATFORMS := darwin/arm64 darwin/amd64 linux/amd64 linux/arm64

.PHONY: build install release clean tidy

## build: build the binary at ./chrono for your platform
build:
	CGO_ENABLED=0 go build -ldflags "$(LDFLAGS)" -trimpath -o chrono ./cmd/chrono

## install: install chrono into $GOBIN (~/go/bin by default)
install:
	CGO_ENABLED=0 go install -ldflags "$(LDFLAGS)" -trimpath ./cmd/chrono

## release: compressed binaries + checksums in ./dist for all platforms
release: clean
	@mkdir -p dist
	@for p in $(PLATFORMS); do \
		os=$${p%/*}; arch=$${p#*/}; name=chrono-$(VERSION)-$$os-$$arch; \
		echo "  -> $$name"; \
		CGO_ENABLED=0 GOOS=$$os GOARCH=$$arch go build -ldflags "$(LDFLAGS)" -trimpath -o dist/$$name/chrono ./cmd/chrono; \
		cp README.md README.es.md dist/$$name/; \
		tar -C dist -czf dist/$$name.tar.gz $$name; \
		rm -rf dist/$$name; \
	done
	@cd dist && shasum -a 256 *.tar.gz > SHA256SUMS
	@echo "Done. Artifacts in ./dist:"; ls -1 dist

## tidy: resolve dependencies
tidy:
	go mod tidy

## clean: remove build artifacts
clean:
	rm -rf dist chrono result
