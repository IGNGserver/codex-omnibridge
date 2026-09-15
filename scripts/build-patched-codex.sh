#!/usr/bin/env bash
set -euo pipefail

STOCK_CODEX_COMMIT="${CODEX_MP_CODEX_COMMIT:-73a1148c9c775c2a4616ce5096291740a00ed68a}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
REPOSITORY="${CODEX_MP_CODEX_REPOSITORY:-https://github.com/openai/codex.git}"
CACHE_DIR="${XDG_CACHE_HOME:-${HOME}/.cache}/codex-multiprovider"
SOURCE_DIR="${CODEX_MP_CODEX_SOURCE_DIR:-${CACHE_DIR}/codex-rs}"
OUTPUT_DIR="${CODEX_MP_CODEX_OUTPUT_DIR:-${PROJECT_DIR}/dist/stock-codex}"
TARGET_DIR="${CODEX_MP_CODEX_TARGET_DIR:-${CACHE_DIR}/target}"
SKIP_FETCH=0
SKIP_BUILD=0

usage() {
  cat >&2 <<EOF
usage: $(basename "$0") [options]

options:
  --source DIR       use an existing Codex checkout or clone destination
  --output DIR       write side-by-side launchers and binaries here
  --target-dir DIR  use this Cargo target directory
  --skip-fetch       do not clone a missing checkout
  --skip-build       package already-built release binaries
  -h, --help         show this help
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source)
      SOURCE_DIR="$2"
      shift 2
      ;;
    --output)
      OUTPUT_DIR="$2"
      shift 2
      ;;
    --target-dir)
      TARGET_DIR="$2"
      shift 2
      ;;
    --skip-fetch)
      SKIP_FETCH=1
      shift
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
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

command -v cargo >/dev/null 2>&1 || {
  echo "error: cargo is required" >&2
  exit 1
}
command -v git >/dev/null 2>&1 || {
  echo "error: git is required" >&2
  exit 1
}

if [[ ! -e "${SOURCE_DIR}" ]]; then
  if [[ "$SKIP_FETCH" == "1" ]]; then
    echo "error: --skip-fetch was requested but the Codex checkout is absent" >&2
    exit 1
  fi
  mkdir -p "$(dirname -- "${SOURCE_DIR}")"
  git clone --filter=blob:none --no-checkout "${REPOSITORY}" "${SOURCE_DIR}"
  git -C "${SOURCE_DIR}" fetch --depth=1 origin "${STOCK_CODEX_COMMIT}"
  git -C "${SOURCE_DIR}" checkout --detach "${STOCK_CODEX_COMMIT}"
fi

if [[ ! -f "${SOURCE_DIR}/codex-rs/Cargo.toml" && ! -f "${SOURCE_DIR}/Cargo.toml" ]]; then
  echo "error: ${SOURCE_DIR} is not a Codex source checkout" >&2
  exit 1
fi

SOURCE_HEAD="$(git -C "${SOURCE_DIR}" rev-parse --verify HEAD)"
if [[ "${SOURCE_HEAD}" != "${STOCK_CODEX_COMMIT}" ]]; then
  printf 'error: expected stock Codex HEAD %s, found %s\n' "${STOCK_CODEX_COMMIT}" "${SOURCE_HEAD}" >&2
  exit 1
fi

PATCH_DIR="${PROJECT_DIR}/patches/codex/${STOCK_CODEX_COMMIT}"
PATCH_APPLIER="${PATCH_DIR}/apply.sh"
if [[ ! -x "${PATCH_APPLIER}" ]]; then
  printf 'error: no verified Codex patch is available for commit %s (%s)\n' "${STOCK_CODEX_COMMIT}" "${PATCH_APPLIER}" >&2
  exit 1
fi
bash "${PATCH_APPLIER}" "${SOURCE_DIR}"

if [[ -f "${SOURCE_DIR}/codex-rs/Cargo.toml" ]]; then
  MANIFEST_PATH="${SOURCE_DIR}/codex-rs/Cargo.toml"
  CARGO_ROOT="${SOURCE_DIR}/codex-rs"
else
  MANIFEST_PATH="${SOURCE_DIR}/Cargo.toml"
  CARGO_ROOT="${SOURCE_DIR}"
fi

if [[ "${SKIP_BUILD}" != "1" ]]; then
  mkdir -p "${TARGET_DIR}"
  CARGO_TARGET_DIR="${TARGET_DIR}" \
  CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
  CARGO_PROFILE_RELEASE_DEBUG="${CARGO_PROFILE_RELEASE_DEBUG:-0}" \
  CARGO_PROFILE_RELEASE_INCREMENTAL="${CARGO_PROFILE_RELEASE_INCREMENTAL:-false}" \
    cargo build --locked --release --manifest-path "${MANIFEST_PATH}" -p codex-cli --bin codex
  CARGO_TARGET_DIR="${TARGET_DIR}" \
  CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
  CARGO_PROFILE_RELEASE_DEBUG="${CARGO_PROFILE_RELEASE_DEBUG:-0}" \
  CARGO_PROFILE_RELEASE_INCREMENTAL="${CARGO_PROFILE_RELEASE_INCREMENTAL:-false}" \
    cargo build --locked --release --manifest-path "${MANIFEST_PATH}" -p codex-app-server --bin codex-app-server
fi

TARGET_SUFFIX=""
if [[ -n "${CARGO_BUILD_TARGET:-}" ]]; then
  TARGET_SUFFIX="/${CARGO_BUILD_TARGET}"
fi
CODEX_BINARY="${TARGET_DIR}${TARGET_SUFFIX}/release/codex"
APP_SERVER_BINARY="${TARGET_DIR}${TARGET_SUFFIX}/release/codex-app-server"
[[ -x "${CODEX_BINARY}" ]] || {
  echo "error: missing release binary ${CODEX_BINARY}" >&2
  exit 1
}
[[ -x "${APP_SERVER_BINARY}" ]] || {
  echo "error: missing release binary ${APP_SERVER_BINARY}" >&2
  exit 1
}

mkdir -p "${OUTPUT_DIR}"
STAGING_DIR="$(mktemp -d "${OUTPUT_DIR}/.staging.XXXXXX")"
cleanup() {
  if [[ -d "${STAGING_DIR}" ]]; then
    find "${STAGING_DIR}" -depth -delete
  fi
}
trap cleanup EXIT

install -m 0755 "${CODEX_BINARY}" "${STAGING_DIR}/codex-mp-codex-bin"
install -m 0755 "${APP_SERVER_BINARY}" "${STAGING_DIR}/codex-mp-app-server-bin"

write_portable_launcher() {
  local launcher_name="$1"
  local binary_name="$2"
  cat >"${STAGING_DIR}/${launcher_name}" <<EOF
#!/usr/bin/env sh
set -eu

SCRIPT_DIR="\$(CDPATH= cd -- "\$(dirname -- "\$0")" && pwd)"
MANAGER_BIN="\${CODEX_MP_MANAGER_BIN:-\${SCRIPT_DIR}/codex-mp}"
if [ ! -x "\${MANAGER_BIN}" ]; then
  MANAGER_BIN="\$(command -v codex-mp || true)"
fi
if [ -z "\${MANAGER_BIN}" ] || [ ! -x "\${MANAGER_BIN}" ]; then
  echo "error: codex-mp manager is required" >&2
  exit 1
fi

if [ -n "\${CODEX_MP_ROUTER_ENDPOINT_FILE:-}" ]; then
  exec "\${MANAGER_BIN}" launch --codex-binary "\${SCRIPT_DIR}/${binary_name}" \
    --endpoint-file "\${CODEX_MP_ROUTER_ENDPOINT_FILE}" -- "\$@"
else
  exec "\${MANAGER_BIN}" launch --codex-binary "\${SCRIPT_DIR}/${binary_name}" -- "\$@"
fi
EOF
  chmod 0755 "${STAGING_DIR}/${launcher_name}"
}

write_portable_launcher codex-mp-codex codex-mp-codex-bin
write_portable_launcher codex-mp-app-server codex-mp-app-server-bin

cat >"${STAGING_DIR}/codex-mp-build.json" <<EOF
{
  "schema_version": 1,
  "upstream_repository": "${REPOSITORY}",
  "upstream_commit": "${STOCK_CODEX_COMMIT}",
  "stock_runtime": true,
  "omni_bridge_provider": "omnibridge",
  "codex_binary": "codex-mp-codex-bin",
  "app_server_binary": "codex-mp-app-server-bin",
  "uses_existing_codex_home": true,
  "official_codex_binary_untouched": true
}
EOF

for artifact in "${STAGING_DIR}"/*; do
  artifact_name="$(basename -- "${artifact}")"
  case "${artifact_name}" in
    codex-mp-codex|codex-mp-app-server|codex-mp-codex-bin|codex-mp-app-server-bin)
      mode=0755
      ;;
    *)
      mode=0644
      ;;
  esac
  install -m "${mode}" "${artifact}" "${OUTPUT_DIR}/${artifact_name}"
done

printf 'stock Codex artifacts written to %s\n' "${OUTPUT_DIR}"
printf 'stock Codex commit: %s\n' "${STOCK_CODEX_COMMIT}"
