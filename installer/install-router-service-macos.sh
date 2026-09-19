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

# Escape a replacement string for use on the right-hand side of `s|..|..|`.
#
# `&` means "the whole match", `\` escapes, and `|` is the delimiter. These paths
# come from the environment (`HOME`, `CODEX_MP_REGISTRY`, ...), so a directory name
# containing `\` was **silently** mangled — the plist would point at a different
# registry than the CLI reads, with no error anywhere.
sed_replacement() {
  printf '%s' "$1" | sed -e 's/[\\&|]/\\&/g'
}

# Render to a temporary file first so a failed render never replaces a plist
# `launchctl` is currently using.
PLIST_TMP="${PLIST_PATH}.tmp"
trap 'rm -f "${PLIST_TMP}"' EXIT
sed \
  -e "s|__CLI_PATH__|$(sed_replacement "${CLI_BINARY}")|g" \
  -e "s|__REGISTRY_PATH__|$(sed_replacement "${REGISTRY_PATH}")|g" \
  -e "s|__ENDPOINT_FILE__|$(sed_replacement "${ENDPOINT_FILE}")|g" \
  -e "s|__LOG_PATH__|$(sed_replacement "${LOG_DIR}/router")|g" \
  "${TEMPLATE}" > "${PLIST_TMP}"

# The template has no equivalent guard on Linux-only placeholders, so verify that
# every placeholder was actually substituted before installing.
if grep -qE '__CLI_PATH__|__REGISTRY_PATH__|__ENDPOINT_FILE__|__LOG_PATH__' "${PLIST_TMP}"; then
  echo "error: failed to substitute every placeholder in ${TEMPLATE}" >&2
  exit 1
fi

install -m 0644 "${PLIST_TMP}" "${PLIST_PATH}"
rm -f "${PLIST_TMP}"
trap - EXIT

if [[ "${CODEX_MP_ENABLE_SERVICE:-1}" == "1" ]]; then
  launchctl load -w "${PLIST_PATH}"
fi

echo "installed ${PLIST_PATH}"
echo "macOS router service configured; logs written to ${LOG_DIR}/router.*.log"
