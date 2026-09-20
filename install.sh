#!/usr/bin/env sh
# chrono installer. Usage: ./install.sh
# Prefers a prebuilt binary from ./dist for your platform; otherwise builds
# from source with Go. Installs to ~/.local/bin (or $PREFIX).
set -eu

PREFIX="${PREFIX:-$HOME/.local/bin}"
VERSION="${VERSION:-v0.1.0}"

os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch=amd64 ;;
  arm64|aarch64) arch=arm64 ;;
esac
case "$os" in darwin) os=darwin ;; linux) os=linux ;; *) echo "unsupported OS: $os"; exit 1 ;; esac

tarball="dist/chrono-$VERSION-$os-$arch.tar.gz"
mkdir -p "$PREFIX"

if [ -f "$tarball" ]; then
  echo "Installing prebuilt binary ($os/$arch)..."
  tmp=$(mktemp -d)
  tar -C "$tmp" -xzf "$tarball"
  install -m 0755 "$tmp"/*/chrono "$PREFIX/chrono"
  rm -rf "$tmp"
elif command -v go >/dev/null 2>&1; then
  echo "No prebuilt binary found; building from source with Go..."
  CGO_ENABLED=0 go build -ldflags "-s -w -X main.version=$VERSION" -trimpath -o "$PREFIX/chrono" ./cmd/chrono
else
  echo "Need either a tarball in ./dist or Go installed. Aborting." >&2
  exit 1
fi

echo "Installed at $PREFIX/chrono"
case ":$PATH:" in
  *":$PREFIX:"*) echo "Ready: '$PREFIX' is already on your PATH." ;;
  *) echo "NOTE: add '$PREFIX' to your PATH:  export PATH=\"$PREFIX:\$PATH\"" ;;
esac
command -v git >/dev/null 2>&1 || echo "NOTE: 'git' is not installed; chrono needs it."
command -v gh  >/dev/null 2>&1 || echo "Note: 'gh' not found; optional, only for reading PRs/issues."
