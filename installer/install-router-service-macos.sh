#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CLI_BINARY="${CODEX_MP_CLI_BINARY:-}"
if [[ -z "${CLI_BINARY}" ]]; then
  if command -v codex-mp >/dev/null 2>&1; then
    CLI_BINARY="$(command -v codex-mp)"
  elif [[ -x "${SCRIPT_DIR}/codex-mp" ]]; then
    CLI_BINARY="${SCRIPT_DIR}/codex-mp"
  elif [[ -x "${HOME}/.local/bin/codex-mp" ]]; then
    CLI_BINARY="${HOME}/.local/bin/codex-mp"
  else
    echo "error: codex-mp binary was not found; please specify CODEX_MP_CLI_BINARY" >&2
    exit 1
  fi
fi

LAUNCH_AGENTS_DIR="${HOME}/Library/LaunchAgents"
PLIST_NAME="dev.codex-multiprovider.router.plist"
PLIST_PATH="${LAUNCH_AGENTS_DIR}/${PLIST_NAME}"
LOG_DIR="${HOME}/Library/Logs/CodexMultiProvider"

CONFIG_DIR="${HOME}/Library/Application Support/dev.codex-multiprovider.Codex MultiProvider"
REGISTRY_PATH="${CODEX_MP_REGISTRY:-${CONFIG_DIR}/providers.json}"
ENDPOINT_FILE="${CODEX_MP_ROUTER_ENDPOINT_FILE:-${CONFIG_DIR}/router-endpoint.json}"

mkdir -p "${LAUNCH_AGENTS_DIR}" "${LOG_DIR}" "$(dirname "${REGISTRY_PATH}")"

# Unload previous service if loaded
launchctl unload "${PLIST_PATH}" 2>/dev/null || true

TEMPLATE="${SCRIPT_DIR}/dev.codex-multiprovider.router.plist"
if [[ ! -f "${TEMPLATE}" ]]; then
  echo "error: plist template not found at ${TEMPLATE}" >&2
  exit 1
fi

sed \
  -e "s|__CLI_PATH__|${CLI_BINARY}|g" \
  -e "s|__REGISTRY_PATH__|${REGISTRY_PATH}|g" \
  -e "s|__ENDPOINT_FILE__|${ENDPOINT_FILE}|g" \
  -e "s|__LOG_PATH__|${LOG_DIR}/router|g" \
  "${TEMPLATE}" > "${PLIST_PATH}"

chmod 0644 "${PLIST_PATH}"

if [[ "${CODEX_MP_ENABLE_SERVICE:-1}" == "1" ]]; then
  launchctl load -w "${PLIST_PATH}"
fi

echo "installed ${PLIST_PATH}"
echo "macOS router service configured; logs written to ${LOG_DIR}/router.*.log"
