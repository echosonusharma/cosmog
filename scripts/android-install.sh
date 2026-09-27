#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<EOF
Usage: $0 [--release] [--no-launch] [--abi <abi>] [--universal]

  Build Cosmog Android APK(s) and install the smallest matching one per device.

  --release    Release build (all ABIs). Default: debug, device ABIs only
  --no-launch  Install only; do not launch the app
  --abi ABI    Override ABI detection (arm64-v8a | armeabi-v7a | x86 | x86_64)
  --universal  Build/install the universal APK (old behavior, ~4x larger)
EOF
  exit 1
}

RELEASE=0
LAUNCH=1
ABI_OVERRIDE=""
UNIVERSAL=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) RELEASE=1; shift ;;
    --no-launch) LAUNCH=0; shift ;;
    --abi)
      if [[ $# -lt 2 ]]; then
        echo "--abi requires a value (arm64-v8a | armeabi-v7a | x86 | x86_64)" >&2
        usage
      fi
      ABI_OVERRIDE="$2"; shift 2 ;;
    --abi=*) ABI_OVERRIDE="${1#--abi=}"; shift ;;
    --universal) UNIVERSAL=1; shift ;;
    -h|--help) usage ;;
    *) echo "Unknown option: $1" >&2; usage ;;
  esac
done

case "$ABI_OVERRIDE" in
  ""|arm64-v8a|armeabi-v7a|x86|x86_64) ;;
  *) echo "Unknown ABI: $ABI_OVERRIDE" >&2; usage ;;
esac

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export JAVA_HOME="${JAVA_HOME:-/usr/lib/jvm/java-17-openjdk}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
export NDK_HOME="${NDK_HOME:-$ANDROID_HOME/ndk/27.1.12297006}"

PACKAGE="com.sonus.cosmog"
APK_BASE="$ROOT/src-tauri/gen/android/app/build/outputs/apk"

if [[ "$RELEASE" -eq 1 ]]; then
  BUILD_TYPE="release"
else
  BUILD_TYPE="debug"
fi

# Android ABI -> Rust target (for `tauri android build --target`).
rust_target_for_abi() {
  case "$1" in
    arm64-v8a) echo "aarch64" ;;
    armeabi-v7a) echo "armv7" ;;
    x86) echo "i686" ;;
    x86_64) echo "x86_64" ;;
    *) echo "" ;;
  esac
}

HAVE_ADB=0
if command -v adb >/dev/null 2>&1; then
  HAVE_ADB=1
fi

# Authorized devices only (state "device"), skip unauthorized/offline.
DEVICES=()
if [[ "$HAVE_ADB" -eq 1 ]]; then
  mapfile -t DEVICES < <(adb devices | awk 'NR>1 && $2=="device" {print $1}')
fi

# Device serial -> ABI, used to compile only needed Rust targets (debug)
# and to install the matching split APK.
declare -A DEVICE_ABI=()
if [[ -n "$ABI_OVERRIDE" ]]; then
  for serial in ${DEVICES[@]+"${DEVICES[@]}"}; do
    DEVICE_ABI["$serial"]="$ABI_OVERRIDE"
  done
elif [[ "$HAVE_ADB" -eq 1 ]]; then
  for serial in ${DEVICES[@]+"${DEVICES[@]}"}; do
    abi="$(adb -s "$serial" shell getprop ro.product.cpu.abi 2>/dev/null | tr -d '\r\n ' || true)"
    if [[ -n "$abi" ]]; then
      DEVICE_ABI["$serial"]="$abi"
    fi
  done
fi

# Unique Rust targets covering all connected devices (debug only).
TARGETS=()
if [[ "$RELEASE" -eq 0 ]]; then
  for serial in ${DEVICES[@]+"${DEVICES[@]}"}; do
    abi="${DEVICE_ABI[$serial]:-}"
    target="$(rust_target_for_abi "$abi")"
    if [[ -n "$target" && ! " ${TARGETS[*]:-} " =~ " $target " ]]; then
      TARGETS+=("$target")
    fi
  done
  if [[ -n "$ABI_OVERRIDE" && ${#TARGETS[@]} -eq 0 ]]; then
    TARGETS=("$(rust_target_for_abi "$ABI_OVERRIDE")")
  fi
  if [[ ${#TARGETS[@]} -eq 0 ]]; then
    TARGETS=("aarch64") # default: covers modern phones when no device is attached
  fi
fi

# Drop stale packaged outputs: Gradle's incremental packager can leave hundreds
# of MB of zero padding inside rebuilt APKs. Deleting them forces a clean
# repackage (seconds) without recompiling Rust/Kotlin.
rm -rf "$APK_BASE"

if [[ "$RELEASE" -eq 1 ]]; then
  echo "Building Android release APKs (all ABIs, split per ABI)..."
  if [[ "$UNIVERSAL" -eq 1 ]]; then
    npm run tauri -- android build --apk
  else
    npm run tauri -- android build --apk --split-per-abi
  fi
else
  echo "Building Android debug APKs (targets: ${TARGETS[*]})..."
  if [[ "$UNIVERSAL" -eq 1 ]]; then
    npm run tauri -- android build --debug --apk --target "${TARGETS[@]}"
  else
    npm run tauri -- android build --debug --apk --split-per-abi --target "${TARGETS[@]}"
  fi
fi

# Tauri names split outputs with short arch names (arm64, not arm64-v8a),
# so probe each alias for the device ABI in turn.
split_names_for_abi() {
  case "$1" in
    arm64-v8a) echo "arm64-v8a arm64 aarch64" ;;
    armeabi-v7a) echo "armeabi-v7a armv7 arm" ;;
    x86) echo "x86 i686" ;;
    x86_64) echo "x86_64 x64" ;;
    *) echo "$1" ;;
  esac
}

# Newest APK inside a split output dir: apk/<split>/<type>/app-<split>-<type>.apk
# Falls back to the universal APK when no split output matches.
apk_for_abi() {
  local abi="$1"
  if [[ "$UNIVERSAL" -eq 0 ]]; then
    local name split_dir newest
    for name in $(split_names_for_abi "$abi"); do
      split_dir="$APK_BASE/$name/$BUILD_TYPE"
      if [[ -d "$split_dir" ]]; then
        newest="$(ls -t "$split_dir"/*.apk 2>/dev/null | head -1 || true)"
        if [[ -n "$newest" ]]; then
          echo "$newest"
          return 0
        fi
      fi
    done
  fi
  echo "$APK_BASE/universal/$BUILD_TYPE/app-universal-$BUILD_TYPE.apk"
}

UNIVERSAL_APK="$(apk_for_abi universal)"
if [[ ! -f "$UNIVERSAL_APK" && "$UNIVERSAL" -eq 1 ]]; then
  echo "Error: APK not found at $UNIVERSAL_APK" >&2
  exit 1
fi
echo "APKs in: $APK_BASE"

if [[ "$HAVE_ADB" -eq 0 ]]; then
  echo "adb not found in PATH — skipping install."
  echo "Install manually with: adb install -r \"\$(ls -t $APK_BASE/*/$BUILD_TYPE/*.apk 2>/dev/null | head -1)\""
  exit 0
fi

if [[ ${#DEVICES[@]} -eq 0 ]]; then
  echo "No adb device connected — skipping install."
  echo "Connect a device (USB debugging) and re-run, or:"
  echo "  adb install -r \"\$(ls -t $APK_BASE/*/$BUILD_TYPE/*.apk | head -1)\""
  exit 0
fi

fail=0
for serial in "${DEVICES[@]}"; do
  abi="${DEVICE_ABI[$serial]:-universal}"
  APK="$(apk_for_abi "$abi")"
  if [[ ! -f "$APK" ]]; then
    echo "No $BUILD_TYPE APK for ABI $abi; falling back to universal." >&2
    APK="$UNIVERSAL_APK"
  fi
  if [[ ! -f "$APK" ]]; then
    echo "Error: APK not found at $APK" >&2
    fail=1
    continue
  fi
  echo "[$serial] Installing $(basename "$APK") ($(du -h "$APK" | cut -f1)) for ABI $abi..."

  # Streamed installs of large APKs flake occasionally; retry a few times.
  installed=0
  attempt=1
  while (( attempt <= 3 )); do
    if adb -s "$serial" install -r "$APK"; then
      installed=1
      break
    fi
    echo "[$serial] Install attempt $attempt failed, retrying..." >&2
    attempt=$((attempt + 1))
    sleep 2
  done
  if [[ "$installed" -eq 0 ]]; then
    echo "[$serial] Install failed after 3 attempts." >&2
    fail=1
    continue
  fi

  if [[ "$LAUNCH" -eq 1 ]]; then
    echo "[$serial] Launching $PACKAGE..."
    adb -s "$serial" shell monkey -p "$PACKAGE" -c android.intent.category.LAUNCHER 1 >/dev/null
  fi
done

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi
echo "Done."
