#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${CODEX_MP_INSTALL_DIR:-${HOME}/.local/bin}"
CLI_BIN="${INSTALL_DIR}/codex-mp"
UNINSTALLER="${INSTALL_DIR}/codex-mp-uninstall"
SERVICE_UNINSTALLER="${INSTALL_DIR}/codex-mp-router-service-uninstall"
MANIFEST="${INSTALL_DIR}/.codex-mp-install-manifest"

[[ -x "${CLI_BIN}" ]] || {
  echo "error: ${CLI_BIN} is missing; refusing to remove Codex integration" >&2
  exit 1
}

if [[ -x "${SERVICE_UNINSTALLER}" ]]; then
  "${SERVICE_UNINSTALLER}" || true
fi

"${CLI_BIN}" uninstall "$@"

if [[ -f "${MANIFEST}" ]]; then
  while IFS= read -r owned_file; do
    [[ -n "${owned_file}" ]] || continue
    [[ "${owned_file}" == "${INSTALL_DIR}/"* ]] || continue
    rm -f -- "${owned_file}"
  done <"${MANIFEST}"
else
  rm -f -- "${CLI_BIN}" "${UNINSTALLER}"
fi

echo "removed Codex MultiProvider binaries and launchers"
