#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${CODEX_MP_INSTALL_DIR:-${HOME}/.local/bin}"
CLI_BIN="${INSTALL_DIR}/codex-mp"
UNINSTALLER="${INSTALL_DIR}/codex-mp-uninstall"
SERVICE_UNINSTALLER="${INSTALL_DIR}/codex-mp-router-service-uninstall"
MANIFEST="${INSTALL_DIR}/.codex-mp-install-manifest"
LAUNCH_AGENT="${HOME}/Library/LaunchAgents/dev.codex-multiprovider.router.plist"

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

if [[ -f "${MANIFEST}" ]]; then
  while IFS= read -r owned_file; do
    [[ -n "${owned_file}" ]] || continue
    [[ "${owned_file}" == "${INSTALL_DIR}/"* ]] || continue
    rm -f -- "${owned_file}"
  done <"${MANIFEST}"
else
  rm -f -- \
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

# The launchd plist lives outside INSTALL_DIR, so it is never covered by the
# manifest and has to be removed explicitly.
if [[ -f "${LAUNCH_AGENT}" ]]; then
  launchctl unload "${LAUNCH_AGENT}" 2>/dev/null || true
  rm -f -- "${LAUNCH_AGENT}"
fi

echo "removed Codex MultiProvider binaries and launchers"
