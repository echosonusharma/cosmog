#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <tag> [semver]"
  echo "  tag     GitHub tag, e.g. v1.1.1"
  echo "  semver  App version (default: tag without leading v)"
  exit 1
}

[[ $# -lt 1 || $# -gt 2 ]] && usage

TAG="$1"
SEMVER="${2:-${TAG#v}}"

# Validate tag format
if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Error: tag must be vX.Y.Z, got '$TAG'" >&2
  exit 1
fi

# Validate semver format
if [[ ! "$SEMVER" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Error: semver must be X.Y.Z, got '$SEMVER'" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "Bumping version to $SEMVER (tag $TAG)..."

# package.json
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$SEMVER\"/" "$ROOT/package.json"

# tauri.conf.json
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$SEMVER\"/" "$ROOT/src-tauri/tauri.conf.json"

# Cargo.toml — only the first [package] version line
sed -i "0,/^version = \"[^\"]*\"/{s/^version = \"[^\"]*\"/version = \"$SEMVER\"/}" "$ROOT/src-tauri/Cargo.toml"

# Update Cargo.lock to reflect new version
cargo update --manifest-path "$ROOT/src-tauri/Cargo.toml" --package cosmog 2>/dev/null || true

git -C "$ROOT" add \
  package.json \
  src-tauri/tauri.conf.json \
  src-tauri/Cargo.toml \
  src-tauri/Cargo.lock

git -C "$ROOT" commit -m "bump version to $TAG"
git -C "$ROOT" tag "$TAG"
git -C "$ROOT" push origin HEAD
git -C "$ROOT" push origin "$TAG"

echo "Released $TAG"
