#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIST_DIR="${ROOT_DIR}/dist"
ARCHES=("amd64" "arm64" "armhf" "riscv64")

if [[ -n "${SYSAIDMIN_ARCHES:-}" ]]; then
  read -r -a ARCHES <<< "${SYSAIDMIN_ARCHES}"
fi

mkdir -p "${DIST_DIR}"
echo "[*] Building multi-arch artifacts into ${DIST_DIR}"

for deb_arch in "${ARCHES[@]}"; do
  dockerfile="${ROOT_DIR}/Dockerfile.${deb_arch}"
  if [[ ! -f "${dockerfile}" ]]; then
    echo "[!] Error: Dockerfile not found: ${dockerfile}"
    exit 1
  fi

  image_name="sysaidmin-deb-${deb_arch}"
  echo "[*] (${deb_arch}) building with Dockerfile.${deb_arch}"
  docker build \
    -f "${dockerfile}" \
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

