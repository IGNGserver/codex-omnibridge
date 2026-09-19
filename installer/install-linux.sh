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
PATCHED_CODEX_BUILD_SCRIPT="${PROJECT_DIR}/scripts/build-patched-codex.sh"
[[ -f "${PATCHED_CODEX_BUILD_SCRIPT}" ]] || PATCHED_CODEX_BUILD_SCRIPT="${PROJECT_DIR}/build-patched-codex.sh"
UNINSTALL_SCRIPT="${PROJECT_DIR}/installer/uninstall-linux.sh"
[[ -f "${UNINSTALL_SCRIPT}" ]] || UNINSTALL_SCRIPT="${PROJECT_DIR}/uninstall-linux.sh"
ROUTER_SERVICE_INSTALLER="${PROJECT_DIR}/installer/install-router-service.sh"
ROUTER_SERVICE_UNINSTALLER="${PROJECT_DIR}/installer/uninstall-router-service.sh"
ROUTER_SERVICE_UNIT="${PROJECT_DIR}/installer/codex-mp-router.service"
[[ -f "${ROUTER_SERVICE_INSTALLER}" ]] || ROUTER_SERVICE_INSTALLER="${PROJECT_DIR}/install-router-service.sh"
[[ -f "${ROUTER_SERVICE_UNINSTALLER}" ]] || ROUTER_SERVICE_UNINSTALLER="${PROJECT_DIR}/uninstall-router-service.sh"
[[ -f "${ROUTER_SERVICE_UNIT}" ]] || ROUTER_SERVICE_UNIT="${PROJECT_DIR}/codex-mp-router.service"
STOCK_CODEX_ARTIFACT_DIR="${CODEX_MP_CODEX_ARTIFACT_DIR:-${PROJECT_DIR}/dist/stock-codex}"
INSTALL_MANIFEST="${INSTALL_DIR}/.codex-mp-install-manifest"

CLI_BINARY="${PROJECT_DIR}/target/release/codex-mp"
BUILD_FROM_SOURCE=0
if [[ -f "${PROJECT_DIR}/Cargo.toml" ]]; then
  BUILD_FROM_SOURCE=1
elif [[ -x "${PROJECT_DIR}/codex-mp" ]]; then
  CLI_BINARY="${PROJECT_DIR}/codex-mp"
else
  echo "error: package does not contain codex-mp and is not a source checkout" >&2
  exit 1
fi

if [[ "${BUILD_FROM_SOURCE}" == "1" ]]; then
  command -v cargo >/dev/null 2>&1 || {
    echo "error: cargo is required when installing from a source checkout" >&2
    exit 1
  }
fi

if [[ -n "${CODEX_MP_NATIVE_SYSROOT:-}" ]]; then
  SYSROOT="${CODEX_MP_NATIVE_SYSROOT}"
  [[ -d "${SYSROOT}" ]] || {
    echo "error: CODEX_MP_NATIVE_SYSROOT does not exist: ${SYSROOT}" >&2
    exit 1
  }
  export PATH="${SYSROOT}/usr/bin:${PATH}"
  export LD_LIBRARY_PATH="${SYSROOT}/usr/lib/x86_64-linux-gnu:${LD_LIBRARY_PATH:-}"
  export PKG_CONFIG_SYSROOT_DIR="${SYSROOT}"
  export PKG_CONFIG_LIBDIR="${SYSROOT}/usr/lib/x86_64-linux-gnu/pkgconfig:${SYSROOT}/usr/lib/pkgconfig:${SYSROOT}/usr/share/pkgconfig:/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/lib/pkgconfig:/usr/share/pkgconfig"
  export PKG_CONFIG_PATH="${PKG_CONFIG_LIBDIR}"
fi

export CARGO_BUILD_JOBS="${CODEX_MP_BUILD_JOBS:-1}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="${CODEX_MP_RELEASE_CODEGEN_UNITS:-256}"
export CARGO_PROFILE_RELEASE_INCREMENTAL="${CODEX_MP_RELEASE_INCREMENTAL:-false}"
export CARGO_PROFILE_RELEASE_LTO="${CODEX_MP_RELEASE_LTO:-false}"
if [[ -n "${CODEX_MP_LINKER:-}" ]]; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="${CODEX_MP_LINKER}"
fi

if [[ "${BUILD_FROM_SOURCE}" == "1" ]]; then
  cargo build --release --locked --manifest-path "${PROJECT_DIR}/Cargo.toml" --package codex-mp-cli
fi

if [[ "${CODEX_MP_BUILD_CODEX:-0}" == "1" ]]; then
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

if [[ -f "${INSTALL_MANIFEST}" ]]; then
  while IFS= read -r owned_file; do
    [[ -n "${owned_file}" ]] || continue
    [[ "${owned_file}" == "${INSTALL_DIR}/"* ]] || continue
    [[ "${owned_file}" == "${INSTALL_MANIFEST}" ]] && continue
    rm -f "${owned_file}"
  done <"${INSTALL_MANIFEST}"
fi

install -m 0755 "${CLI_BINARY}" "${INSTALL_DIR}/codex-mp"
install -m 0755 "${UNINSTALL_SCRIPT}" "${INSTALL_DIR}/codex-mp-uninstall"
install -m 0755 "${ROUTER_SERVICE_INSTALLER}" "${INSTALL_DIR}/codex-mp-router-service-install"
install -m 0755 "${ROUTER_SERVICE_UNINSTALLER}" "${INSTALL_DIR}/codex-mp-router-service-uninstall"
install -m 0644 "${ROUTER_SERVICE_UNIT}" "${INSTALL_DIR}/codex-mp-router.service"

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

# The manifest is written before the optional post-install steps below so that a
# failure there can never leave files on disk that the uninstaller cannot see.
manifest_tmp="${INSTALL_MANIFEST}.tmp"
{
  printf '%s\n' \
    "${INSTALL_DIR}/codex-mp" \
    "${INSTALL_DIR}/codex-mp-uninstall" \
    "${INSTALL_DIR}/codex-mp-router-service-install" \
    "${INSTALL_DIR}/codex-mp-router-service-uninstall" \
    "${INSTALL_DIR}/codex-mp-router.service"
  if [[ "${install_stock_codex}" == "1" ]]; then
    printf '%s\n' \
      "${INSTALL_DIR}/codex-mp-codex-bin" \
      "${INSTALL_DIR}/codex-mp-codex" \
      "${INSTALL_DIR}/codex-mp-app-server-bin" \
      "${INSTALL_DIR}/codex-mp-app-server" \
      "${INSTALL_DIR}/codex-mp-build.json"
  fi
  printf '%s\n' "${INSTALL_MANIFEST}"
} >"${manifest_tmp}"
install -m 0600 "${manifest_tmp}" "${INSTALL_MANIFEST}"
rm -f "${manifest_tmp}"

if [[ "${CODEX_MP_INSTALL_SERVICE:-0}" == "1" ]]; then
  CODEX_MP_ENABLE_SERVICE="${CODEX_MP_ENABLE_SERVICE:-0}" \
    CODEX_MP_UNIT_SOURCE="${INSTALL_DIR}/codex-mp-router.service" \
    "${INSTALL_DIR}/codex-mp-router-service-install"
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

echo "installed ${INSTALL_DIR}/codex-mp"
echo "installed ${INSTALL_DIR}/codex-mp-uninstall"
echo "installed ${INSTALL_DIR}/codex-mp-router-service-install"
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
