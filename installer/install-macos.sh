#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
if [[ ! -f "${PROJECT_DIR}/Cargo.toml" && -x "${SCRIPT_DIR}/codex-mp" ]]; then
  # Release archives place the executable and installer scripts together at
  # the archive root; source checkouts keep this script below the workspace.
  PROJECT_DIR="${SCRIPT_DIR}"
fi
INSTALL_DIR="${CODEX_MP_INSTALL_DIR:-${HOME}/.local/bin}"
CLI_SOURCE="${CODEX_MP_CLI_BINARY:-${PROJECT_DIR}/codex-mp}"
UNINSTALL_SOURCE="${PROJECT_DIR}/uninstall-macos.sh"
[[ -f "${UNINSTALL_SOURCE}" ]] || UNINSTALL_SOURCE="${SCRIPT_DIR}/uninstall-macos.sh"
ROUTER_SERVICE_INSTALLER="${PROJECT_DIR}/installer/install-router-service-macos.sh"
ROUTER_SERVICE_UNINSTALLER="${PROJECT_DIR}/installer/uninstall-router-service-macos.sh"
[[ -f "${ROUTER_SERVICE_INSTALLER}" ]] || ROUTER_SERVICE_INSTALLER="${PROJECT_DIR}/install-router-service-macos.sh"
[[ -f "${ROUTER_SERVICE_UNINSTALLER}" ]] || ROUTER_SERVICE_UNINSTALLER="${PROJECT_DIR}/uninstall-router-service-macos.sh"
ROUTER_SERVICE_PLIST="${PROJECT_DIR}/installer/dev.codex-multiprovider.router.plist"
[[ -f "${ROUTER_SERVICE_PLIST}" ]] || ROUTER_SERVICE_PLIST="${PROJECT_DIR}/dev.codex-multiprovider.router.plist"
PATCHED_CODEX_BUILD_SCRIPT="${PROJECT_DIR}/scripts/build-patched-codex.sh"
[[ -f "${PATCHED_CODEX_BUILD_SCRIPT}" ]] || PATCHED_CODEX_BUILD_SCRIPT="${PROJECT_DIR}/build-patched-codex.sh"
STOCK_CODEX_ARTIFACT_DIR="${CODEX_MP_CODEX_ARTIFACT_DIR:-${PROJECT_DIR}/dist/stock-codex}"
MANIFEST="${INSTALL_DIR}/.codex-mp-install-manifest"

if [[ ! -x "${CLI_SOURCE}" && -f "${PROJECT_DIR}/Cargo.toml" ]]; then
  command -v cargo >/dev/null 2>&1 || {
    echo "error: cargo is required when installing from a source checkout" >&2
    exit 1
  }
  cargo build --release --locked --manifest-path "${PROJECT_DIR}/Cargo.toml" --package codex-mp-cli
  CLI_SOURCE="${PROJECT_DIR}/target/release/codex-mp"
fi
[[ -x "${CLI_SOURCE}" ]] || {
  echo "error: macOS package does not contain an executable codex-mp: ${CLI_SOURCE}" >&2
  exit 1
}
[[ -f "${UNINSTALL_SOURCE}" ]] || {
  echo "error: uninstall-macos.sh is missing from the package" >&2
  exit 1
}

if [[ "${CODEX_MP_BUILD_CODEX:-0}" == "1" ]]; then
  [[ -f "${PATCHED_CODEX_BUILD_SCRIPT}" ]] || {
    echo "error: build-patched-codex.sh is missing from the package" >&2
    exit 1
  }
  "${PATCHED_CODEX_BUILD_SCRIPT}" --output "${STOCK_CODEX_ARTIFACT_DIR}"
fi

install_stock_codex=0
if [[ "${CODEX_MP_BUILD_CODEX:-0}" == "1" || -n "${CODEX_MP_CODEX_ARTIFACT_DIR:-}" ]]; then
  install_stock_codex=1
  for artifact in \
    codex-mp-codex-bin \
    codex-mp-codex \
    codex-mp-app-server-bin \
    codex-mp-app-server \
    codex-mp-build.json; do
    if [[ ! -f "${STOCK_CODEX_ARTIFACT_DIR}/${artifact}" ]]; then
      printf 'error: stock Codex artifact is missing: %s\n' "${STOCK_CODEX_ARTIFACT_DIR}/${artifact}" >&2
      exit 1
    fi
  done
fi

mkdir -p "${INSTALL_DIR}"
if [[ -f "${MANIFEST}" ]]; then
  while IFS= read -r owned_file; do
    [[ -n "${owned_file}" ]] || continue
    [[ "${owned_file}" == "${INSTALL_DIR}/"* ]] || continue
    [[ "${owned_file}" == "${MANIFEST}" ]] && continue
    rm -f -- "${owned_file}"
  done <"${MANIFEST}"
fi

install -m 0755 "${CLI_SOURCE}" "${INSTALL_DIR}/codex-mp"
install -m 0755 "${UNINSTALL_SOURCE}" "${INSTALL_DIR}/codex-mp-uninstall"
if [[ -f "${ROUTER_SERVICE_INSTALLER}" ]]; then
  install -m 0755 "${ROUTER_SERVICE_INSTALLER}" "${INSTALL_DIR}/codex-mp-router-service-install"
  install -m 0755 "${ROUTER_SERVICE_UNINSTALLER}" "${INSTALL_DIR}/codex-mp-router-service-uninstall"
  install -m 0644 "${ROUTER_SERVICE_PLIST}" "${INSTALL_DIR}/dev.codex-multiprovider.router.plist"
fi

if [[ "${CODEX_MP_INSTALL_SERVICE:-0}" == "1" && -x "${INSTALL_DIR}/codex-mp-router-service-install" ]]; then
  CODEX_MP_ENABLE_SERVICE="${CODEX_MP_ENABLE_SERVICE:-0}" \
    "${INSTALL_DIR}/codex-mp-router-service-install"
fi

if [[ "${install_stock_codex}" == "1" ]]; then
  for artifact in \
    codex-mp-codex-bin \
    codex-mp-codex \
    codex-mp-app-server-bin \
    codex-mp-app-server \
    codex-mp-build.json; do
    mode=0644
    [[ "${artifact}" == *-bin || "${artifact}" == codex-mp-codex || "${artifact}" == codex-mp-app-server ]] && mode=0755
    install -m "${mode}" "${STOCK_CODEX_ARTIFACT_DIR}/${artifact}" "${INSTALL_DIR}/${artifact}"
  done
fi

if [[ "${CODEX_MP_INSTALL_DESKTOP:-0}" == "1" ]]; then
  if [[ "${install_stock_codex}" != "1" ]]; then
    echo "error: CODEX_MP_INSTALL_DESKTOP=1 requires the explicit experimental Desktop runtime artifact" >&2
    exit 1
  fi
  "${INSTALL_DIR}/codex-mp" desktop install \
    --app-server-binary "${INSTALL_DIR}/codex-mp-app-server-bin" \
    --codex-mp-binary "${INSTALL_DIR}/codex-mp"
fi

manifest_tmp="${MANIFEST}.tmp"
{
  printf '%s\n' \
    "${INSTALL_DIR}/codex-mp" \
    "${INSTALL_DIR}/codex-mp-uninstall"
  if [[ -f "${ROUTER_SERVICE_INSTALLER}" ]]; then
    printf '%s\n' \
      "${INSTALL_DIR}/codex-mp-router-service-install" \
      "${INSTALL_DIR}/codex-mp-router-service-uninstall" \
      "${INSTALL_DIR}/dev.codex-multiprovider.router.plist"
  fi
  if [[ "${install_stock_codex}" == "1" ]]; then
    printf '%s\n' \
      "${INSTALL_DIR}/codex-mp-codex-bin" \
      "${INSTALL_DIR}/codex-mp-codex" \
      "${INSTALL_DIR}/codex-mp-app-server-bin" \
      "${INSTALL_DIR}/codex-mp-app-server" \
      "${INSTALL_DIR}/codex-mp-build.json"
  fi
  printf '%s\n' "${MANIFEST}"
} >"${manifest_tmp}"
install -m 0600 "${manifest_tmp}" "${MANIFEST}"
rm -f -- "${manifest_tmp}"

echo "installed ${INSTALL_DIR}/codex-mp"
echo "installed ${INSTALL_DIR}/codex-mp-uninstall"
if [[ "${install_stock_codex}" == "1" ]]; then
  echo "installed stock Codex launchers in ${INSTALL_DIR}"
else
  echo "stock Codex artifact install skipped (set CODEX_MP_BUILD_CODEX=1 or CODEX_MP_CODEX_ARTIFACT_DIR=...)"
fi
if [[ "${CODEX_MP_INSTALL_DESKTOP:-0}" == "1" ]]; then
  echo "installed ChatGPT Desktop runtime adapter; restart ChatGPT Desktop"
fi
if [[ ":${PATH}:" != *":${INSTALL_DIR}:"* ]]; then
  echo "note: add ${INSTALL_DIR} to PATH before running codex-mp"
fi
