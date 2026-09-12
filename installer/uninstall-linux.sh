#!/usr/bin/env bash
set -euo pipefail

INSTALL_DIR="${CODEX_MP_INSTALL_DIR:-${HOME}/.local/bin}"
CLI_BIN="${INSTALL_DIR}/codex-mp"
PANEL_BIN="${INSTALL_DIR}/codex-mp-panel"
UNINSTALLER="${INSTALL_DIR}/codex-mp-uninstall"
INSTALL_MANIFEST="${INSTALL_DIR}/.codex-mp-install-manifest"
DESKTOP_FILE_NAME="codex-multiprovider-panel.desktop"
APPLICATIONS_FILE="${HOME}/.local/share/applications/${DESKTOP_FILE_NAME}"
AUTOSTART_FILE="${HOME}/.config/autostart/${DESKTOP_FILE_NAME}"

if [[ ! -x "${CLI_BIN}" ]]; then
  echo "error: ${CLI_BIN} is missing; refusing to remove Codex integration" >&2
  exit 1
fi

panel_pids() {
  ps -eo pid=,uid=,comm= \
    | awk -v current_uid="$(id -u)" '$2 == current_uid && $3 == "codex-mp-panel" {print $1}'
}

stop_panel() {
  local pids
  pids="$(panel_pids)"
  if [[ -z "${pids}" ]]; then
    return 0
  fi
  kill -TERM ${pids}
  for _ in $(seq 1 20); do
    [[ -z "$(panel_pids)" ]] && return 0
    sleep 0.1
  done
  pids="$(panel_pids)"
  if [[ -n "${pids}" ]]; then
    kill -KILL ${pids}
  fi
}

stop_panel
"${CLI_BIN}" uninstall "$@"

remove_owned_desktop_file() {
  local path="$1"
  if [[ ! -f "${path}" ]]; then
    return 0
  fi
  if grep -Fxq "Name=Codex MultiProvider" "${path}" \
    && grep -Fxq "Exec=${PANEL_BIN}" "${path}"; then
    rm -f "${path}"
  else
    echo "preserved non-MultiProvider desktop file: ${path}" >&2
  fi
}

remove_owned_desktop_file "${APPLICATIONS_FILE}"
remove_owned_desktop_file "${AUTOSTART_FILE}"

if [[ -f "${INSTALL_MANIFEST}" ]]; then
  while IFS= read -r owned_file; do
    [[ -n "${owned_file}" ]] || continue
    [[ "${owned_file}" == "${INSTALL_DIR}/"* ]] || continue
    rm -f "${owned_file}"
  done <"${INSTALL_MANIFEST}"
else
  rm -f \
    "${PANEL_BIN}" \
    "${CLI_BIN}" \
    "${UNINSTALLER}" \
    "${INSTALL_DIR}/codex-mp-codex-bin" \
    "${INSTALL_DIR}/codex-mp-codex" \
    "${INSTALL_DIR}/codex-mp-app-server-bin" \
    "${INSTALL_DIR}/codex-mp-app-server" \
    "${INSTALL_DIR}/codex-mp-build.json"
fi

echo "removed Codex MultiProvider binaries and launchers"
