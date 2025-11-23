#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIST_DIR="${ROOT_DIR}/dist"
ARCH_MATRIX=(
  "amd64:x86_64-unknown-linux-gnu"
  "arm64:aarch64-unknown-linux-gnu"
  "armhf:armv7-unknown-linux-gnueabihf"
  "riscv64:riscv64gc-unknown-linux-gnu"
)

if [[ -n "${SYSAIDMIN_ARCH_MATRIX:-}" ]]; then
  IFS=' ' read -r -a ARCH_MATRIX <<< "${SYSAIDMIN_ARCH_MATRIX}"
fi

REQUESTED_ARCHES=()
if [[ -n "${SYSAIDMIN_ARCHES:-}" ]]; then
  read -r -a REQUESTED_ARCHES <<< "${SYSAIDMIN_ARCHES}"
fi

mkdir -p "${DIST_DIR}"
echo "[*] Building multi-arch artifacts into ${DIST_DIR}"

for entry in "${ARCH_MATRIX[@]}"; do
  IFS=':' read -r deb_arch rust_target <<< "${entry}"
  if [[ ${#REQUESTED_ARCHES[@]} -gt 0 ]]; then
    skip=true
    for requested in "${REQUESTED_ARCHES[@]}"; do
      if [[ "${requested}" == "${deb_arch}" ]]; then
        skip=false
        break
      fi
    done
    $skip && continue
  fi

  image_name="sysaidmin-deb-${deb_arch}"
  echo "[*] (${deb_arch}) building via ${rust_target}"
  docker build \
    --build-arg "DEB_ARCH=${deb_arch}" \
    --build-arg "RUST_TARGET=${rust_target}" \
    -f "${ROOT_DIR}/Dockerfile.debian12" \
    -t "${image_name}" \
    "${ROOT_DIR}"

  cid=$(docker create "${image_name}")
  arch_dir="${DIST_DIR}/${deb_arch}"
  rm -rf "${arch_dir}"
  mkdir -p "${arch_dir}"
  docker cp "${cid}:/app/artifacts/${deb_arch}/." "${arch_dir}/"
  docker rm "${cid}" >/dev/null

  echo "[*] (${deb_arch}) artifacts copied to ${arch_dir}"
done

echo "[*] Done. Check ${DIST_DIR}/<arch> for binaries and .deb packages."

