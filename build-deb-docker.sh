#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE_NAME="sysaidmin-deb"
DIST_DIR="${ROOT_DIR}/dist"

echo "[*] Building Debian 12 image..."
docker build -f "${ROOT_DIR}/Dockerfile.debian12" -t "${IMAGE_NAME}" "${ROOT_DIR}"

CID=$(docker create "${IMAGE_NAME}")
mkdir -p "${DIST_DIR}"
echo "[*] Copying artifacts to dist/"
docker cp "${CID}:/app/target/debian" "${DIST_DIR}/"
docker rm "${CID}" >/dev/null

echo "[*] Debian packages available under dist/debian"

