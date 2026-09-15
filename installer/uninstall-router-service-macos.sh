#!/usr/bin/env bash
set -euo pipefail

LAUNCH_AGENTS_DIR="${HOME}/Library/LaunchAgents"
PLIST_NAME="dev.codex-multiprovider.router.plist"
PLIST_PATH="${LAUNCH_AGENTS_DIR}/${PLIST_NAME}"

if [[ -f "${PLIST_PATH}" ]]; then
  launchctl unload -w "${PLIST_PATH}" 2>/dev/null || true
  rm -f -- "${PLIST_PATH}"
  echo "removed ${PLIST_PATH}; provider registry and credentials were preserved"
else
  echo "macOS router service was not installed"
fi
