#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${CODEX_MP_INSTALL_DIR:-${HOME}/.local/bin}"
CLI_BIN="${INSTALL_DIR}/codex-mp"
UNINSTALLER="${INSTALL_DIR}/codex-mp-uninstall"
SERVICE_UNINSTALLER="${INSTALL_DIR}/codex-mp-router-service-uninstall"
INSTALL_MANIFEST="${INSTALL_DIR}/.codex-mp-install-manifest"

# A half-finished or partially removed install is exactly the case the manifest
# exists for, so never abort just because the CLI binary is gone: degrade to
# manifest-driven cleanup instead.
if [[ -x "${SERVICE_UNINSTALLER}" ]]; then
  "${SERVICE_UNINSTALLER}" || true
fi

if [[ -x "${CLI_BIN}" ]]; then
  "${CLI_BIN}" uninstall "$@"
else
  echo "warning: ${CLI_BIN} is missing; skipping Codex integration restore" >&2
  echo "warning: re-install codex-mp and run 'codex-mp uninstall' to restore config.toml" >&2
fi

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
    "${INSTALL_DIR}/codex-mp-router-service-install" \
    "${INSTALL_DIR}/codex-mp-router.service" \
    "${INSTALL_DIR}/codex-mp-codex-bin" \
    "${INSTALL_DIR}/codex-mp-codex" \
    "${INSTALL_DIR}/codex-mp-app-server-bin" \
    "${INSTALL_DIR}/codex-mp-app-server" \
    "${INSTALL_DIR}/codex-mp-build.json"
fi

echo "removed Codex MultiProvider binaries and launchers"
