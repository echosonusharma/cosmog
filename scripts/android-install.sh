#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<EOF
Usage: $0 [--release] [--no-launch]

  Build Cosmog Android APK and install via adb when a device is connected.

  --release    Release build (all ABIs). Default: debug aarch64
  --no-launch  Install only; do not launch the app
EOF
  exit 1
}

RELEASE=0
LAUNCH=1
for arg in "$@"; do
  case "$arg" in
    --release) RELEASE=1 ;;
    --no-launch) LAUNCH=0 ;;
    -h|--help) usage ;;
    *) echo "Unknown option: $arg" >&2; usage ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export JAVA_HOME="${JAVA_HOME:-/usr/lib/jvm/java-17-openjdk}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
export NDK_HOME="${NDK_HOME:-$ANDROID_HOME/ndk/27.1.12297006}"

PACKAGE="com.sonus.cosmog"
APK_DEBUG="$ROOT/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk"
APK_RELEASE="$ROOT/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk"

if [[ "$RELEASE" -eq 1 ]]; then
  echo "Building Android release APK..."
  npm run tauri -- android build --apk
  APK="$APK_RELEASE"
else
  echo "Building Android debug APK (aarch64)..."
  npm run tauri -- android build --debug --apk --target aarch64
  APK="$APK_DEBUG"
fi

if [[ ! -f "$APK" ]]; then
  echo "Error: APK not found at $APK" >&2
  exit 1
fi

echo "Built: $APK"

if ! command -v adb >/dev/null 2>&1; then
  echo "Error: adb not found in PATH" >&2
  exit 1
fi

# Authorized devices only (state "device"), skip unauthorized/offline
mapfile -t DEVICES < <(adb devices | awk 'NR>1 && $2=="device" {print $1}')

if [[ ${#DEVICES[@]} -eq 0 ]]; then
  echo "No adb device connected — skipping install."
  echo "Connect a device (USB debugging) and re-run, or:"
  echo "  adb install -r \"$APK\""
  exit 0
fi

echo "Device(s): ${DEVICES[*]}"
echo "Installing..."
adb install -r "$APK"

if [[ "$LAUNCH" -eq 1 ]]; then
  echo "Launching $PACKAGE..."
  adb shell monkey -p "$PACKAGE" -c android.intent.category.LAUNCHER 1 >/dev/null
fi

echo "Done."
