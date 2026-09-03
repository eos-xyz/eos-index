#!/usr/bin/env bash
# build-release.sh — produce distributable gitindex binaries locally (no CI).
#
# macOS hosts build the two macOS arches natively; Linux binaries are built in a
# rust container when a Docker daemon is reachable (skipped with a note if not).
# Outputs tar.gz + SHA256SUMS to eng/gitindex/dist/. For the full cross-platform
# matrix (incl. Windows) use the release workflow (.github/workflows/
# release-gitindex.yml) on a `gitindex-v*` tag.
#
#   ./scripts/build-release.sh

set -euo pipefail
cd "$(dirname "$0")/.."          # eng/gitindex
DIST="$PWD/dist"
rm -rf "$DIST"; mkdir -p "$DIST"

pack() { # pack <binary> <name>
  local bin="$1" name="$2" dir; dir="$(dirname "$bin")"
  tar czf "$DIST/gitindex-$name.tar.gz" -C "$dir" "$(basename "$bin")"
  echo "  ✓ $name"
}

echo "▶ macOS (native + cross target)"
if [ "$(uname -s)" = "Darwin" ]; then
  host_arch=$([ "$(uname -m)" = "arm64" ] && echo aarch64 || echo x86_64)
  cargo build --release >/dev/null
  pack "target/release/gitindex" "macos-$([ "$host_arch" = aarch64 ] && echo arm64 || echo amd64)"
  other=$([ "$host_arch" = aarch64 ] && echo x86_64-apple-darwin || echo aarch64-apple-darwin)
  rustup target add "$other" >/dev/null 2>&1 || true
  if cargo build --release --target "$other" >/dev/null 2>&1; then
    pack "target/$other/release/gitindex" "macos-$([ "$other" = x86_64-apple-darwin ] && echo amd64 || echo arm64)"
  else echo "  ⚠ skipped $other (cross build failed)"; fi
else
  echo "  (not on macOS — skipping macOS binaries)"
fi

echo "▶ Linux (via Docker, if a daemon is reachable)"
if docker info >/dev/null 2>&1; then
  for pair in "linux/arm64:arm64" "linux/amd64:amd64"; do
    plat="${pair%%:*}"; tag="${pair##*:}"
    docker run --rm --platform "$plat" -v "$PWD":/build -v "$DIST":/out -w /build \
      -e CARGO_TARGET_DIR=/tmp/target rust:1-slim bash -c \
      "apt-get update -qq && apt-get install -y -qq cmake pkg-config libssl-dev git >/dev/null 2>&1 && cargo build --release >/dev/null && cp /tmp/target/release/gitindex /out/gitindex-linux-$tag-bin" \
      && (mkdir -p "$DIST/tmp-$tag" && mv "$DIST/gitindex-linux-$tag-bin" "$DIST/tmp-$tag/gitindex" \
          && tar czf "$DIST/gitindex-linux-$tag.tar.gz" -C "$DIST/tmp-$tag" gitindex && rm -rf "$DIST/tmp-$tag" \
          && echo "  ✓ linux-$tag") \
      || echo "  ⚠ linux-$tag build failed"
  done
else
  echo "  (no Docker daemon — Linux binaries come from the release workflow)"
fi

( cd "$DIST" && shasum -a 256 *.tar.gz > SHA256SUMS 2>/dev/null || true )
echo "▶ done → $DIST"
ls -1 "$DIST"
