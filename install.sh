#!/usr/bin/env sh
# chrono installer.
#
# One-liner (no clone needed):
#   curl -fsSL https://raw.githubusercontent.com/juan52878911/chrono/main/install.sh | sh
#
# From a checkout:
#   ./install.sh
#
# Order of preference:
#   1. Prebuilt tarball in ./dist for your platform (when run from a checkout)
#   2. Download the matching binary from the GitHub release + verify SHA256
#   3. Build from source with Go
#
# Env overrides:
#   VERSION=v0.1.0   pin a release (default: latest)
#   PREFIX=/usr/local/bin   install dir (default: /usr/local/bin if writable, else ~/.local/bin)
set -eu

REPO="juan52878911/chrono"
VERSION="${VERSION:-}"

# --- pick an install dir on PATH -------------------------------------------
if [ -z "${PREFIX:-}" ]; then
  if [ -w /usr/local/bin ] 2>/dev/null; then
    PREFIX=/usr/local/bin
  else
    PREFIX="$HOME/.local/bin"
  fi
fi

# --- detect platform --------------------------------------------------------
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch=amd64 ;;
  arm64|aarch64) arch=arm64 ;;
  *) echo "unsupported arch: $arch" >&2; exit 1 ;;
esac
case "$os" in
  darwin) os=darwin ;;
  linux)  os=linux ;;
  *) echo "unsupported OS: $os" >&2; exit 1 ;;
esac

mkdir -p "$PREFIX"

# --- helpers ----------------------------------------------------------------
have() { command -v "$1" >/dev/null 2>&1; }

fetch() { # fetch URL -> stdout
  if have curl; then curl -fsSL "$1"
  elif have wget; then wget -qO- "$1"
  else echo "need curl or wget to download" >&2; return 1
  fi
}
fetch_to() { # fetch URL FILE
  if have curl; then curl -fsSL "$1" -o "$2"
  elif have wget; then wget -qO "$2" "$1"
  else echo "need curl or wget to download" >&2; return 1
  fi
}

resolve_latest() { # print latest tag by following the /releases/latest redirect
  url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO/releases/latest" 2>/dev/null) || return 1
  # .../releases/tag/vX.Y.Z  ->  vX.Y.Z
  echo "$url" | sed -n 's#.*/tag/##p'
}

verify_sha() { # verify_sha FILE SUMS_FILE   (SUMS lines: "<sha>  <name>")
  file=$1; sums=$2; name=$(basename "$file")
  want=$(grep " $name\$" "$sums" | awk '{print $1}' | head -n1)
  [ -n "$want" ] || { echo "no checksum for $name; skipping verify" >&2; return 0; }
  if have shasum; then got=$(shasum -a 256 "$file" | awk '{print $1}')
  elif have sha256sum; then got=$(sha256sum "$file" | awk '{print $1}')
  else echo "no sha256 tool; skipping verify" >&2; return 0
  fi
  [ "$want" = "$got" ] || { echo "checksum mismatch for $name" >&2; return 1; }
  echo "Checksum OK ($name)"
}

install_from_tarball() { # install_from_tarball TARBALL
  tmp=$(mktemp -d)
  tar -C "$tmp" -xzf "$1"
  # binary may be at <tmp>/chrono or <tmp>/<dir>/chrono
  bin=$(find "$tmp" -type f -name chrono | head -n1)
  [ -n "$bin" ] || { echo "chrono binary not found in tarball" >&2; rm -rf "$tmp"; exit 1; }
  install -m 0755 "$bin" "$PREFIX/chrono"
  rm -rf "$tmp"
}

# --- 1) local tarball from a checkout ---------------------------------------
localtar="dist/chrono-${VERSION:-v0.1.0}-$os-$arch.tar.gz"
if [ -f "$localtar" ]; then
  echo "Installing prebuilt binary from $localtar ($os/$arch)..."
  install_from_tarball "$localtar"

# --- 2) download from the GitHub release ------------------------------------
elif have curl || have wget; then
  if [ -z "$VERSION" ]; then
    VERSION=$(resolve_latest || true)
    [ -n "$VERSION" ] || VERSION="v0.1.0"
  fi
  asset="chrono-$VERSION-$os-$arch.tar.gz"
  base="https://github.com/$REPO/releases/download/$VERSION"
  echo "Downloading $asset from release $VERSION..."
  tmp=$(mktemp -d)
  fetch_to "$base/$asset" "$tmp/$asset"
  if fetch "$base/SHA256SUMS" > "$tmp/SHA256SUMS" 2>/dev/null; then
    verify_sha "$tmp/$asset" "$tmp/SHA256SUMS"
  else
    echo "SHA256SUMS not available; skipping verify" >&2
  fi
  install_from_tarball "$tmp/$asset"
  rm -rf "$tmp"

# --- 3) build from source ---------------------------------------------------
elif have go; then
  echo "No prebuilt binary and no downloader; building from source with Go..."
  CGO_ENABLED=0 go build -ldflags "-s -w -X main.version=${VERSION:-dev}" -trimpath \
    -o "$PREFIX/chrono" ./cmd/chrono
else
  echo "Need curl/wget (to download) or Go (to build). Aborting." >&2
  exit 1
fi

# --- done -------------------------------------------------------------------
echo "Installed at $PREFIX/chrono"
"$PREFIX/chrono" --version 2>/dev/null | head -n1 || true
case ":$PATH:" in
  *":$PREFIX:"*) echo "Ready: '$PREFIX' is already on your PATH." ;;
  *) echo "NOTE: add '$PREFIX' to your PATH:  export PATH=\"$PREFIX:\$PATH\"" ;;
esac
have git || echo "NOTE: 'git' is not installed; chrono needs it."
have gh  || echo "Note: 'gh' not found; optional, only for reading PRs/issues."
