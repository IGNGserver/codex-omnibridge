#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
UNIT_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"
UNIT_PATH="${UNIT_DIR}/codex-mp-router.service"

# The release installer copies this script, its uninstaller and the unit into the
# same installation directory, so the unit normally sits next to this script.
# CODEX_MP_UNIT_SOURCE lets a caller point at the unit explicitly.
UNIT_SOURCE="${CODEX_MP_UNIT_SOURCE:-${SCRIPT_DIR}/codex-mp-router.service}"

# The unit is a template: it carries __CLI_PATH__ / __REGISTRY_PATH__ /
# __ENDPOINT_FILE__ placeholders that must be substituted at install time.
# Copying it verbatim leaves systemd pointing at paths that do not exist (or at
# another user's registry), which makes the router start "healthy" while
# routing nothing. Mirror install-router-service-macos.sh's sed substitution.
resolve_cli_binary() {
  if [[ -n "${CODEX_MP_CLI_BINARY:-}" ]]; then
    printf '%s\n' "${CODEX_MP_CLI_BINARY}"
    return 0
  fi
  if command -v codex-mp >/dev/null 2>&1; then
    command -v codex-mp
    return 0
  fi
  if [[ -x "${SCRIPT_DIR}/codex-mp" ]]; then
    printf '%s\n' "${SCRIPT_DIR}/codex-mp"
    return 0
  fi
  if [[ -x "${HOME}/.local/bin/codex-mp" ]]; then
    printf '%s\n' "${HOME}/.local/bin/codex-mp"
    return 0
  fi
  echo "error: codex-mp binary was not found; please specify CODEX_MP_CLI_BINARY" >&2
  return 1
}

command -v systemctl >/dev/null 2>&1 || {
  echo "error: systemctl is required for the systemd user service" >&2
  exit 1
}

if [[ ! -f "${UNIT_SOURCE}" ]]; then
  echo "error: systemd unit not found at ${UNIT_SOURCE}" >&2
  echo "hint: pass CODEX_MP_UNIT_SOURCE=/path/to/codex-mp-router.service" >&2
  exit 1
fi

CLI_BINARY="${CODEX_MP_CLI_BINARY:-}"
if [[ -z "${CLI_BINARY}" ]]; then
  CLI_BINARY="$(resolve_cli_binary)" || exit 1
fi
if [[ ! -x "${CLI_BINARY}" ]]; then
  echo "error: codex-mp binary is not executable: ${CLI_BINARY}" >&2
  echo "hint: pass CODEX_MP_CLI_BINARY=/path/to/codex-mp" >&2
  exit 1
fi

# Defaults must match the path the CLI itself resolves, otherwise an installer
# that does not override CODEX_MP_REGISTRY wires the service to a registry the
# CLI never reads: the router starts "healthy" and routes nothing.
#
# The CLI uses ProjectDirs::from("dev", "codex-multiprovider", "Codex MultiProvider")
# (see `codex_mp_core::default_registry_path`). On Linux the `directories` crate
# builds config_dir as `$XDG_CONFIG_HOME/<lowercased application>`, i.e. the
# *application* segment only -- the qualifier/organization are dropped. Verified
# against the real binary: `codex-mp status` reports
# `$HOME/.config/codexmultiprovider/providers.json`.
CONFIG_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/codexmultiprovider"
REGISTRY_PATH="${CODEX_MP_REGISTRY:-${CONFIG_DIR}/providers.json}"
ENDPOINT_FILE="${CODEX_MP_ROUTER_ENDPOINT_FILE:-${CONFIG_DIR}/router-endpoint.json}"

mkdir -p "${UNIT_DIR}" "$(dirname "${REGISTRY_PATH}")"

# Guard the invariant that R-11a was about: the source must be the *unrendered*
# template. A stale/hand-edited unit with hardcoded %h/... paths would install
# "successfully" while the router silently routes nothing.
if ! grep -qF '__CLI_PATH__' "${UNIT_SOURCE}"; then
  echo "error: ${UNIT_SOURCE} is not the systemd unit template (missing __CLI_PATH__)" >&2
  echo "hint: pass the unrendered codex-mp-router.service via CODEX_MP_UNIT_SOURCE" >&2
  exit 1
fi

# Escape a replacement string for use on the right-hand side of `s|..|..|`.
#
# `&` means "the whole match", `\` escapes, and `|` is the delimiter. Paths here
# come from the environment (`HOME`, `XDG_CONFIG_HOME`, `CODEX_MP_REGISTRY`, ...),
# so they are *not* installer-controlled: a directory whose name contains `&` used
# to render `__REGISTRY_PATH__` back into the output (caught by the check below),
# and one containing `\` was **silently** mangled — `/home/a\b/reg` rendered as
# `/home/ab/reg`, the placeholder check passed, and the service pointed at the
# wrong registry. Escape all three so any path round-trips byte-for-byte.
sed_replacement() {
  printf '%s' "$1" | sed -e 's/[\\&|]/\\&/g'
}

CLI_BINARY_ESCAPED="$(sed_replacement "${CLI_BINARY}")"
REGISTRY_PATH_ESCAPED="$(sed_replacement "${REGISTRY_PATH}")"
ENDPOINT_FILE_ESCAPED="$(sed_replacement "${ENDPOINT_FILE}")"

# Render to a temporary file first so a failed render never overwrites the unit
# systemd is currently using.
UNIT_TMP="${UNIT_PATH}.tmp"
trap 'rm -f "${UNIT_TMP}"' EXIT
sed \
  -e "s|__CLI_PATH__|${CLI_BINARY_ESCAPED}|g" \
  -e "s|__REGISTRY_PATH__|${REGISTRY_PATH_ESCAPED}|g" \
  -e "s|__ENDPOINT_FILE__|${ENDPOINT_FILE_ESCAPED}|g" \
  "${UNIT_SOURCE}" > "${UNIT_TMP}"

if grep -qE '__CLI_PATH__|__REGISTRY_PATH__|__ENDPOINT_FILE__' "${UNIT_TMP}"; then
  echo "error: failed to substitute every placeholder in ${UNIT_SOURCE}" >&2
  exit 1
fi

install -m 0644 "${UNIT_TMP}" "${UNIT_PATH}"
rm -f "${UNIT_TMP}"
trap - EXIT

systemctl --user daemon-reload

if [[ "${CODEX_MP_ENABLE_SERVICE:-1}" == "1" ]]; then
  systemctl --user enable --now codex-mp-router.service
fi

echo "installed ${UNIT_PATH}"
echo "router state remains in the managed registry directory; auth.json is untouched"
