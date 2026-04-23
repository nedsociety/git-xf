#!/usr/bin/env sh
set -e

REPO="nedsociety/git-xf"
BIN="git-xf"

# Resolve install directory: honour $INSTALL_DIR, then prefer /usr/local/bin if
# writable, otherwise fall back to ~/.local/bin (added to PATH by most modern
# distros and shells).
if [ -z "$INSTALL_DIR" ]; then
  if [ -w /usr/local/bin ]; then
    INSTALL_DIR=/usr/local/bin
  else
    INSTALL_DIR="$HOME/.local/bin"
  fi
fi

# Detect OS
case "$(uname -s)" in
  Linux)  _os="unknown-linux-musl" ;;
  Darwin) _os="apple-darwin" ;;
  *)
    echo "Unsupported OS: $(uname -s)" >&2
    exit 1
    ;;
esac

# Detect CPU architecture
case "$(uname -m)" in
  x86_64)          _arch="x86_64" ;;
  arm64 | aarch64) _arch="aarch64" ;;
  *)
    echo "Unsupported architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

TARGET="${_arch}-${_os}"

# Resolve version (latest if not pinned via $VERSION)
if [ -z "$VERSION" ]; then
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | awk -F'"' '/tag_name/{print $4; exit}')
fi

if [ -z "$VERSION" ]; then
  echo "Could not determine the latest release version." >&2
  echo "Set VERSION=vX.Y.Z to install a specific version." >&2
  exit 1
fi

URL="https://github.com/${REPO}/releases/download/${VERSION}/${BIN}-${TARGET}.tar.gz"

echo "Installing ${BIN} ${VERSION} (${TARGET}) → ${INSTALL_DIR}/${BIN}"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "$URL" | tar xz -C "$TMP"

mkdir -p "$INSTALL_DIR"
if [ -w "$INSTALL_DIR" ]; then
  install -m 755 "$TMP/$BIN" "$INSTALL_DIR/$BIN"
else
  sudo install -m 755 "$TMP/$BIN" "$INSTALL_DIR/$BIN"
fi

echo "Done. Run: git xf --help"
