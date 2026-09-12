#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PATCH_DIR="${PROJECT_DIR}/patches/codex/73a1148c9c775c2a4616ce5096291740a00ed68a"
EXPECTED_COMMIT="73a1148c9c775c2a4616ce5096291740a00ed68a"
REPOSITORY="${CODEX_MP_CODEX_REPOSITORY:-https://github.com/openai/codex.git}"
CACHE_DIR="${XDG_CACHE_HOME:-${HOME}/.cache}/codex-multiprovider"
SOURCE_DIR="${CODEX_MP_CODEX_SOURCE_DIR:-${CACHE_DIR}/codex-rs}"
OUTPUT_DIR="${CODEX_MP_CODEX_OUTPUT_DIR:-${PROJECT_DIR}/dist/patched-codex}"
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

if [[ ! -x "${PATCH_DIR}/apply.sh" ]]; then
  echo "error: the pinned Codex patch is missing" >&2
  exit 1
fi

if [[ ! -e "${SOURCE_DIR}" ]]; then
  if [[ "$SKIP_FETCH" == "1" ]]; then
    echo "error: --skip-fetch was requested but the Codex checkout is absent" >&2
    exit 1
  fi
  mkdir -p "$(dirname -- "${SOURCE_DIR}")"
  git clone --filter=blob:none --no-checkout "${REPOSITORY}" "${SOURCE_DIR}"
  git -C "${SOURCE_DIR}" fetch --depth=1 origin "${EXPECTED_COMMIT}"
  git -C "${SOURCE_DIR}" checkout --detach "${EXPECTED_COMMIT}"
fi

if [[ ! -f "${SOURCE_DIR}/codex-rs/Cargo.toml" && ! -f "${SOURCE_DIR}/Cargo.toml" ]]; then
  echo "error: ${SOURCE_DIR} is not a Codex source checkout" >&2
  exit 1
fi

SOURCE_HEAD="$(git -C "${SOURCE_DIR}" rev-parse --verify HEAD)"
if [[ "${SOURCE_HEAD}" != "${EXPECTED_COMMIT}" ]]; then
  printf 'error: expected Codex HEAD %s, found %s\n' "${EXPECTED_COMMIT}" "${SOURCE_HEAD}" >&2
  exit 1
fi

"${PATCH_DIR}/apply.sh" "${SOURCE_DIR}"
git -C "${SOURCE_DIR}" -c core.whitespace=error diff --check

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

write_launcher() {
  local launcher_name="$1"
  local binary_name="$2"
  cat >"${STAGING_DIR}/${launcher_name}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="\$(cd -- "\$(dirname -- "\${BASH_SOURCE[0]}")" && pwd)"
REAL_BINARY="\${SCRIPT_DIR}/${binary_name}"
ENDPOINT_FILE="\${CODEX_MP_ROUTER_ENDPOINT_FILE:-\${XDG_CONFIG_HOME:-\${HOME}/.config}/codexmultiprovider/router-endpoint.json}"
MANAGER_PID=""

cleanup() {
  if [[ -n "\${MANAGER_PID}" ]]; then
    kill "\${MANAGER_PID}" 2>/dev/null || true
    wait "\${MANAGER_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

router_is_healthy() {
  [[ -f "\${ENDPOINT_FILE}" ]] || return 1
  local base_url host port status
  base_url="\$(sed -nE 's/.*"base_url"[[:space:]]*:[[:space:]]*"([^"]+)".*/\\1/p' "\${ENDPOINT_FILE}" | head -n 1)"
  [[ "\${base_url}" =~ ^http://([^/:]+):([0-9]+)$ ]] || return 1
  host="\${BASH_REMATCH[1]}"
  port="\${BASH_REMATCH[2]}"
  if ! exec 9<>"/dev/tcp/\${host}/\${port}"; then
    return 1
  fi
  printf 'GET /healthz HTTP/1.1\\r\\nHost: %s\\r\\nConnection: close\\r\\n\\r\\n' "\${host}" >&9
  IFS= read -r -t 1 status <&9 || {
    exec 9>&-
    return 1
  }
  exec 9>&-
  [[ "\${status}" =~ ^HTTP/[0-9.]+[[:space:]]+200[[:space:]] ]]
}

if ! router_is_healthy; then
  MANAGER_BIN="\${CODEX_MP_MANAGER_BIN:-\${SCRIPT_DIR}/codex-mp}"
  if [[ ! -x "\${MANAGER_BIN}" ]]; then
    MANAGER_BIN="\$(command -v codex-mp || true)"
  fi
  if [[ -z "\${MANAGER_BIN}" || ! -x "\${MANAGER_BIN}" ]]; then
    echo "error: codex-mp manager is required when the Router is not already running" >&2
    exit 1
  fi
  mkdir -p "\$(dirname -- "\${ENDPOINT_FILE}")"
  "\${MANAGER_BIN}" manager --endpoint-file "\${ENDPOINT_FILE}" >/dev/null 2>&1 &
  MANAGER_PID=\$!
  for _ in \$(seq 1 50); do
    [[ -f "\${ENDPOINT_FILE}" ]] && break
    kill -0 "\${MANAGER_PID}" 2>/dev/null || break
    sleep 0.1
  done
  if [[ ! -f "\${ENDPOINT_FILE}" ]]; then
    echo "error: local Router did not publish ${binary_name} endpoint" >&2
    exit 1
  fi
fi

export CODEX_MP_ROUTER_ENDPOINT_FILE="\${ENDPOINT_FILE}"
"\${REAL_BINARY}" "\$@"
EOF
  chmod 0755 "${STAGING_DIR}/${launcher_name}"
}

write_launcher codex-mp-codex codex-mp-codex-bin
write_launcher codex-mp-app-server codex-mp-app-server-bin

PATCH_SHA256="$(sha256sum "${PATCH_DIR}/0001-per-turn-local-router.patch" | awk '{print $1}')"
cat >"${STAGING_DIR}/codex-mp-build.json" <<EOF
{
  "schema_version": 1,
  "upstream_repository": "${REPOSITORY}",
  "upstream_commit": "${EXPECTED_COMMIT}",
  "patch_sha256": "${PATCH_SHA256}",
  "codex_binary": "codex-mp-codex-bin",
  "app_server_binary": "codex-mp-app-server-bin",
  "uses_existing_codex_home": true,
  "official_codex_binary_untouched": true
}
EOF

for artifact in "${STAGING_DIR}"/*; do
  install -m "$(stat -c '%a' "${artifact}")" "${artifact}" "${OUTPUT_DIR}/$(basename -- "${artifact}")"
done

printf 'patched Codex artifacts written to %s\n' "${OUTPUT_DIR}"
printf 'upstream commit: %s\n' "${EXPECTED_COMMIT}"
printf 'patch sha256: %s\n' "${PATCH_SHA256}"
