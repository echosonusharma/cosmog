#!/usr/bin/env bash
# Interactive release: bump version, tag (with optional description), push.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

bold() { printf '\033[1m%s\033[0m' "$1"; }
die() { echo "Error: $*" >&2; exit 1; }

# ── preflight ────────────────────────────────────────────────────────────────
[[ -n "$(git status --porcelain)" ]] && die "working tree is not clean, commit or stash first"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
git fetch --tags --quiet origin 2>/dev/null || echo "Warning: could not fetch tags from origin"

LAST_TAG="$(git tag --list 'v*' --sort=-v:refname | head -1)"
CUR_VER="$(sed -n 's/.*"version": "\([^"]*\)".*/\1/p' package.json | head -1)"

# Suggest next patch from the newest tag (or current version).
BASE="${LAST_TAG#v}"; BASE="${BASE:-$CUR_VER}"
IFS=. read -r MA MI PA <<<"$BASE"
SUGGEST="$MA.$MI.$((PA + 1))"

echo
echo "  Branch:          $(bold "$BRANCH")"
echo "  Last release:    $(bold "${LAST_TAG:-none}")"
echo "  Current version: $(bold "$CUR_VER")"
echo

# ── version ──────────────────────────────────────────────────────────────────
read -r -p "New version [$SUGGEST]: " INPUT
SEMVER="${INPUT:-$SUGGEST}"
SEMVER="${SEMVER#v}"
[[ "$SEMVER" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be X.Y.Z, got '$SEMVER'"
TAG="v$SEMVER"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && die "tag $TAG already exists"
if [[ -n "$LAST_TAG" ]]; then
  NEWEST="$(printf '%s\n%s\n' "${LAST_TAG#v}" "$SEMVER" | sort -V | tail -1)"
  [[ "$NEWEST" == "$SEMVER" ]] || die "$SEMVER is not newer than $LAST_TAG"
fi

# ── description ──────────────────────────────────────────────────────────────
echo
echo "Release description (optional). Shown at the top of the release notes."
echo "Type lines, finish with an empty line. Leave empty to skip."
DESC=""
while IFS= read -r -p "> " LINE; do
  [[ -z "$LINE" ]] && break
  DESC+="${DESC:+$'\n'}$LINE"
done

# ── summary ──────────────────────────────────────────────────────────────────
echo
echo "────────────────────────────────────────────"
echo "  Version:  $(bold "$SEMVER")   Tag: $(bold "$TAG")"
if [[ -n "$DESC" ]]; then
  echo "  Description:"
  sed 's/^/    /' <<<"$DESC"
else
  echo "  Description: (none)"
fi
echo "  Commits since ${LAST_TAG:-start}:"
if [[ -n "$LAST_TAG" ]]; then
  git log --pretty=format:'    - %s' "$LAST_TAG..HEAD"; echo
else
  git log -20 --pretty=format:'    - %s'; echo
fi
echo "────────────────────────────────────────────"
[[ "$BRANCH" != "master" ]] && echo "Warning: you are on '$BRANCH', not master."
read -r -p "Bump, tag and push $TAG to origin? [y/N] " OK
[[ "$OK" =~ ^[Yy]$ ]] || { echo "Aborted, nothing changed."; exit 0; }

# ── bump ─────────────────────────────────────────────────────────────────────
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$SEMVER\"/" package.json
sed -i "s/\"version\": \"[^\"]*\"/\"version\": \"$SEMVER\"/" src-tauri/tauri.conf.json
# only the first [package] version line
sed -i "0,/^version = \"[^\"]*\"/{s/^version = \"[^\"]*\"/version = \"$SEMVER\"/}" src-tauri/Cargo.toml
cargo update --manifest-path src-tauri/Cargo.toml --package cosmog 2>/dev/null || true

git add package.json src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "chore: bump version to $TAG"

# Annotated tag carries the description; CI reads it into the release notes.
if [[ -n "$DESC" ]]; then
  git tag -a "$TAG" -m "$DESC"
else
  git tag "$TAG"
fi

git push origin HEAD
git push origin "$TAG"

echo
echo "Released $TAG. Watch the build: gh run list --workflow release.yml -L 1"
