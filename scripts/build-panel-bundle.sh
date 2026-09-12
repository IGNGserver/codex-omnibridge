#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PANEL_DIR="${PROJECT_DIR}/apps/panel"
TAURI_DIR="${PANEL_DIR}/src-tauri"
CLI_BINARY="${PROJECT_DIR}/target/release/codex-mp"
OUTPUT_DIR="${CODEX_MP_PANEL_OUTPUT_DIR:-${PROJECT_DIR}/dist/panel}"
TAURI_CLI_VERSION="${CODEX_MP_TAURI_CLI_VERSION:-2.8.4}"
BUNDLES="${CODEX_MP_PANEL_BUNDLES:-deb}"

usage() {
  cat >&2 <<EOF
usage: $(basename "$0") [options]

options:
  --output DIR       copy generated bundles to DIR
  --bundles LIST     bundle list accepted by Tauri (default: deb)
  --help             show this help

environment:
  CODEX_MP_NATIVE_SYSROOT  optional user-local GTK/WebKit sysroot
  CODEX_MP_TAURI_CLI_VERSION  Tauri CLI npm version (default: ${TAURI_CLI_VERSION})
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output)
      OUTPUT_DIR="$2"
      shift 2
      ;;
    --bundles)
      BUNDLES="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'error: unknown option: %s\n' "$1" >&2
      usage
      exit 64
      ;;
  esac
done

command -v npx >/dev/null 2>&1 || {
  echo "error: npx is required to run the pinned Tauri CLI" >&2
  exit 1
}
command -v cargo >/dev/null 2>&1 || {
  echo "error: cargo is required to build the bundled Router CLI" >&2
  exit 1
}

if [[ -n "${CODEX_MP_NATIVE_SYSROOT:-}" ]]; then
  SYSROOT="${CODEX_MP_NATIVE_SYSROOT}"
  [[ -d "${SYSROOT}" ]] || {
    echo "error: CODEX_MP_NATIVE_SYSROOT does not exist: ${SYSROOT}" >&2
    exit 1
  }
  export PATH="${SYSROOT}/usr/bin:${PATH}"
  export LD_LIBRARY_PATH="${SYSROOT}/usr/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH:-}"
  export PKG_CONFIG_SYSROOT_DIR="${SYSROOT}"
  export PKG_CONFIG_LIBDIR="${SYSROOT}/usr/lib/x86_64-linux-gnu/pkgconfig:${SYSROOT}/usr/lib/pkgconfig:${SYSROOT}/usr/share/pkgconfig:/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/lib/pkgconfig:/usr/share/pkgconfig"
  export PKG_CONFIG_PATH="${PKG_CONFIG_LIBDIR}"
fi

export CARGO_BUILD_JOBS="${CODEX_MP_BUILD_JOBS:-1}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="${CODEX_MP_RELEASE_CODEGEN_UNITS:-256}"
export CARGO_PROFILE_RELEASE_INCREMENTAL="${CODEX_MP_RELEASE_INCREMENTAL:-false}"
export CARGO_PROFILE_RELEASE_LTO="${CODEX_MP_RELEASE_LTO:-false}"
if [[ -n "${CODEX_MP_LINKER:-}" ]]; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="${CODEX_MP_LINKER}"
fi

echo "building bundled Router CLI"
cargo build --release --locked --manifest-path "${PROJECT_DIR}/Cargo.toml" --package codex-mp-cli
[[ -x "${CLI_BINARY}" ]] || {
  echo "error: bundled Router CLI was not produced: ${CLI_BINARY}" >&2
  exit 1
}

mkdir -p "${OUTPUT_DIR}"
(cd "${PANEL_DIR}" && npx --yes "@tauri-apps/cli@${TAURI_CLI_VERSION}" build --ci --bundles "${BUNDLES}")

BUNDLE_DIR="${TAURI_DIR}/target/release/bundle"
mapfile -d '' artifacts < <(
  find "${BUNDLE_DIR}" -type f \( -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' \) -print0
)
if [[ "${#artifacts[@]}" -eq 0 ]]; then
  echo "error: Tauri did not produce a supported bundle in ${BUNDLE_DIR}" >&2
  exit 1
fi

STAGING_DIR="$(mktemp -d "${OUTPUT_DIR}/.staging.XXXXXX")"
cleanup() {
  if [[ -d "${STAGING_DIR}" ]]; then
    find "${STAGING_DIR}" -depth -delete
  fi
}
trap cleanup EXIT

for artifact in "${artifacts[@]}"; do
  install -m 0644 "${artifact}" "${STAGING_DIR}/$(basename -- "${artifact}")"
done
for artifact in "${STAGING_DIR}"/*; do
  install -m 0644 "${artifact}" "${OUTPUT_DIR}/$(basename -- "${artifact}")"
done

printf 'panel bundles written to %s\n' "${OUTPUT_DIR}"
printf 'bundles: %s\n' "${BUNDLES}"
