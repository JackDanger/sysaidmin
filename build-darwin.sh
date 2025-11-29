#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIST_DIR="${ROOT_DIR}/dist"
TARGET="darwin-arm64"
DOCKERFILE="${ROOT_DIR}/Dockerfile.${TARGET}"
RUST_TARGET="aarch64-apple-darwin"

# Check if we're on macOS - if so, build natively (much easier and more reliable)
if [[ "$(uname)" == "Darwin" ]]; then
  echo "[*] Detected macOS - building natively (recommended)"
  echo "[*] Building macOS ARM64 binary into ${DIST_DIR}"
  
  # Ensure Rust target is installed
  if ! rustup target list --installed | grep -q "^${RUST_TARGET}$"; then
    echo "[*] Installing Rust target ${RUST_TARGET}..."
    rustup target add ${RUST_TARGET}
  fi
  
  # Build
  cd "${ROOT_DIR}"
  cargo build --workspace --locked --release --target ${RUST_TARGET}
  
  # Copy artifacts
  arch_dir="${DIST_DIR}/${TARGET}"
  rm -rf "${arch_dir}"
  mkdir -p "${arch_dir}/bin"
  cp target/${RUST_TARGET}/release/sysaidmin "${arch_dir}/bin/sysaidmin"
  
  echo "[*] macOS ARM64 binary copied to ${arch_dir}"
  echo "[*] Done. Binary available at: ${arch_dir}/bin/sysaidmin"
  exit 0
fi

# Fall back to Docker build (note: osxcross has limited ARM64 SDK support)
echo "[*] Not on macOS - attempting Docker build (may have limitations)"
echo "[*] Building macOS ARM64 binary into ${DIST_DIR}"

# Check if macOS SDK is provided for Docker build
if [[ -z "${MACOS_SDK_URL:-}" ]] && [[ ! -f "${ROOT_DIR}/MacOSX.sdk.tar.xz" ]]; then
  echo "[!] Error: macOS SDK required for Docker-based macOS builds"
  echo ""
  echo "Note: osxcross (used in Docker) has very limited ARM64 SDK support."
  echo "For best results, build natively on macOS instead."
  echo ""
  echo "If you must use Docker, provide one of:"
  echo "  1. Copy MacOSX.sdk.tar.xz to the project root, then run:"
  echo "     ./build-darwin.sh"
  echo ""
  echo "  2. Provide MACOS_SDK_URL environment variable:"
  echo "     MACOS_SDK_URL=https://url/to/MacOSX.sdk.tar.xz ./build-darwin.sh"
  echo ""
  echo "The macOS SDK can be obtained from:"
  echo "  - Xcode Command Line Tools (on macOS)"
  echo "  - Apple Developer portal"
  exit 1
fi

mkdir -p "${DIST_DIR}"

if [[ ! -f "${DOCKERFILE}" ]]; then
  echo "[!] Error: Dockerfile not found: ${DOCKERFILE}"
  exit 1
fi

image_name="sysaidmin-${TARGET}"
echo "[*] Building with Dockerfile.${TARGET}"

# Build with macOS SDK
build_args=()
if [[ -n "${MACOS_SDK_URL:-}" ]]; then
  build_args+=(--build-arg "MACOS_SDK_URL=${MACOS_SDK_URL}")
fi

docker build \
  "${build_args[@]}" \
  -f "${DOCKERFILE}" \
  -t "${image_name}" \
  "${ROOT_DIR}"

cid=$(docker create "${image_name}")
arch_dir="${DIST_DIR}/${TARGET}"
rm -rf "${arch_dir}"
mkdir -p "${arch_dir}"
docker cp "${cid}:/app/artifacts/${TARGET}/." "${arch_dir}/"
docker rm "${cid}" >/dev/null

echo "[*] macOS ARM64 binary copied to ${arch_dir}"
echo "[*] Done. Binary available at: ${arch_dir}/bin/sysaidmin"

