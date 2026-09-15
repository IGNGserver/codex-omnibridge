#!/usr/bin/env bash
set -euo pipefail

UNIT_PATH="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user/codex-mp-router.service"
if command -v systemctl >/dev/null 2>&1; then
  systemctl --user disable --now codex-mp-router.service 2>/dev/null || true
  systemctl --user daemon-reload || true
fi
if [[ -f "${UNIT_PATH}" ]]; then
  rm -f -- "${UNIT_PATH}"
  if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload || true
  fi
  echo "removed ${UNIT_PATH}; provider registry and credentials were preserved"
else
  echo "router service was not installed"
fi
