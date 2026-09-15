#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${CODEX_MP_INSTALL_DIR:-${HOME}/.local/bin}"
CLI_BIN="${INSTALL_DIR}/codex-mp"
UNINSTALLER="${INSTALL_DIR}/codex-mp-uninstall"
SERVICE_UNINSTALLER="${INSTALL_DIR}/codex-mp-router-service-uninstall"
INSTALL_MANIFEST="${INSTALL_DIR}/.codex-mp-install-manifest"

if [[ ! -x "${CLI_BIN}" ]]; then
  echo "error: ${CLI_BIN} is missing; refusing to remove Codex integration" >&2
  exit 1
fi

if [[ -x "${SERVICE_UNINSTALLER}" ]]; then
  "${SERVICE_UNINSTALLER}" || true
fi
"${CLI_BIN}" uninstall "$@"

if [[ -f "${INSTALL_MANIFEST}" ]]; then
  while IFS= read -r owned_file; do
    [[ -n "${owned_file}" ]] || continue
    [[ "${owned_file}" == "${INSTALL_DIR}/"* ]] || continue
    rm -f "${owned_file}"
  done <"${INSTALL_MANIFEST}"
else
  rm -f \
    "${CLI_BIN}" \
    "${UNINSTALLER}" \
    "${SERVICE_UNINSTALLER}" \
    "${INSTALL_DIR}/codex-mp-codex-bin" \
    "${INSTALL_DIR}/codex-mp-codex" \
    "${INSTALL_DIR}/codex-mp-app-server-bin" \
    "${INSTALL_DIR}/codex-mp-app-server" \
    "${INSTALL_DIR}/codex-mp-build.json"
fi

echo "removed Codex MultiProvider binaries and launchers"
