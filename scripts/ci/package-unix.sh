#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 <tag> <target-triple> <binary-path> <output-dir>" >&2
  exit 1
fi

TAG="$1"
TARGET_TRIPLE="$2"
BINARY_PATH="$3"
OUTPUT_DIR="$4"

APP_NAME="polyphony"
STAGING_DIR="${OUTPUT_DIR}/${APP_NAME}-${TAG}-${TARGET_TRIPLE}"
ARCHIVE_PATH="${OUTPUT_DIR}/${APP_NAME}-${TAG}-${TARGET_TRIPLE}.tar.gz"

mkdir -p "${STAGING_DIR}/bin"
install -m 0755 "${BINARY_PATH}" "${STAGING_DIR}/bin/${APP_NAME}"
cp README.md "${STAGING_DIR}/README.md"
if [[ -f LICENSE ]]; then
  cp LICENSE "${STAGING_DIR}/LICENSE"
elif [[ -f LICENSE.md ]]; then
  cp LICENSE.md "${STAGING_DIR}/LICENSE"
else
  echo "warning: no LICENSE or LICENSE.md found, skipping license bundle"
fi

CHANGELOG_PATH="${POLYPHONY_CHANGELOG_PATH:-}"
if [[ -n "${CHANGELOG_PATH}" && -f "${CHANGELOG_PATH}" ]]; then
  cp "${CHANGELOG_PATH}" "${STAGING_DIR}/CHANGELOG.md"
  echo "bundled changelog from ${CHANGELOG_PATH}"
elif [[ -n "${CHANGELOG_PATH}" ]]; then
  echo "warning: changelog not found at ${CHANGELOG_PATH}, skipping bundle"
else
  echo "note: POLYPHONY_CHANGELOG_PATH not set, skipping changelog bundle"
fi

tar -C "${OUTPUT_DIR}" -czf "${ARCHIVE_PATH}" "$(basename "${STAGING_DIR}")"

echo "${ARCHIVE_PATH}"
