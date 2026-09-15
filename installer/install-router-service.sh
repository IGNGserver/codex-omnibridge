#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
UNIT_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"
UNIT_PATH="${UNIT_DIR}/codex-mp-router.service"

command -v systemctl >/dev/null 2>&1 || {
  echo "error: systemctl is required for the systemd user service" >&2
  exit 1
}
mkdir -p "${UNIT_DIR}"
install -m 0644 "${SCRIPT_DIR}/codex-mp-router.service" "${UNIT_PATH}"
systemctl --user daemon-reload

if [[ "${CODEX_MP_ENABLE_SERVICE:-1}" == "1" ]]; then
  systemctl --user enable --now codex-mp-router.service
fi

echo "installed ${UNIT_PATH}"
echo "router state remains in the managed registry directory; auth.json is untouched"
