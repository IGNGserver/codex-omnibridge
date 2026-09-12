#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
INSTALL_DIR="${CODEX_MP_INSTALL_DIR:-${HOME}/.local/bin}"
PANEL_MANIFEST="${PROJECT_DIR}/apps/panel/src-tauri/Cargo.toml"
PANEL_BINARY="${PROJECT_DIR}/apps/panel/src-tauri/target/release/codex-mp-panel"
PATCHED_CODEX_BUILD_SCRIPT="${PROJECT_DIR}/scripts/build-patched-codex.sh"
PATCHED_CODEX_ARTIFACT_DIR="${CODEX_MP_CODEX_ARTIFACT_DIR:-${PROJECT_DIR}/dist/patched-codex}"
APPLICATIONS_DIR="${HOME}/.local/share/applications"
AUTOSTART_DIR="${HOME}/.config/autostart"
DESKTOP_FILE_NAME="codex-multiprovider-panel.desktop"
INSTALL_MANIFEST="${INSTALL_DIR}/.codex-mp-install-manifest"

command -v cargo >/dev/null 2>&1 || {
  echo "error: cargo is required" >&2
  exit 1
}

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

cargo build --release --locked --manifest-path "${PROJECT_DIR}/Cargo.toml" --package codex-mp-cli
if [[ "${CODEX_MP_SKIP_PANEL:-0}" != "1" ]]; then
  cargo build --release --locked --manifest-path "${PANEL_MANIFEST}"
fi

if [[ "${CODEX_MP_BUILD_CODEX:-0}" == "1" ]]; then
  "${PATCHED_CODEX_BUILD_SCRIPT}" --output "${PATCHED_CODEX_ARTIFACT_DIR}"
fi

install_patched_codex=0
if [[ "${CODEX_MP_BUILD_CODEX:-0}" == "1" || -n "${CODEX_MP_CODEX_ARTIFACT_DIR:-}" ]]; then
  install_patched_codex=1
  for artifact in \
    codex-mp-codex-bin \
    codex-mp-codex \
    codex-mp-app-server-bin \
    codex-mp-app-server \
    codex-mp-build.json; do
    if [[ ! -f "${PATCHED_CODEX_ARTIFACT_DIR}/${artifact}" ]]; then
      printf 'error: patched Codex artifact is missing: %s\n' "${PATCHED_CODEX_ARTIFACT_DIR}/${artifact}" >&2
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

install -m 0755 "${PROJECT_DIR}/target/release/codex-mp" "${INSTALL_DIR}/codex-mp"
install -m 0755 "${PROJECT_DIR}/installer/uninstall-linux.sh" "${INSTALL_DIR}/codex-mp-uninstall"

if [[ "${CODEX_MP_SKIP_PANEL:-0}" != "1" ]]; then
  install -m 0755 "${PANEL_BINARY}" "${INSTALL_DIR}/codex-mp-panel"
  mkdir -p "${APPLICATIONS_DIR}"
  desktop_file="${APPLICATIONS_DIR}/${DESKTOP_FILE_NAME}"
  desktop_tmp="${desktop_file}.tmp"
  cat >"${desktop_tmp}" <<EOF
[Desktop Entry]
Type=Application
Name=Codex MultiProvider
Comment=Provider and model manager for Codex
Exec=${INSTALL_DIR}/codex-mp-panel
Terminal=false
Categories=Development;
StartupNotify=true
EOF
  install -m 0644 "${desktop_tmp}" "${desktop_file}"
  rm -f "${desktop_tmp}"

  if [[ "${CODEX_MP_AUTOSTART:-1}" == "1" ]]; then
    mkdir -p "${AUTOSTART_DIR}"
    autostart_file="${AUTOSTART_DIR}/${DESKTOP_FILE_NAME}"
    autostart_tmp="${autostart_file}.tmp"
    cat >"${autostart_tmp}" <<EOF
[Desktop Entry]
Type=Application
Name=Codex MultiProvider
Comment=Provider and model manager for Codex
Exec=${INSTALL_DIR}/codex-mp-panel
Terminal=false
Categories=Development;
StartupNotify=false
X-GNOME-Autostart-enabled=true
EOF
    install -m 0644 "${autostart_tmp}" "${autostart_file}"
    rm -f "${autostart_tmp}"
  fi
fi

if [[ "${install_patched_codex}" == "1" ]]; then
  for artifact in \
    codex-mp-codex-bin \
    codex-mp-codex \
    codex-mp-app-server-bin \
    codex-mp-app-server \
    codex-mp-build.json; do
    mode=0644
    [[ "${artifact}" == *-bin || "${artifact}" == codex-mp-codex || "${artifact}" == codex-mp-app-server ]] && mode=0755
    install -m "${mode}" "${PATCHED_CODEX_ARTIFACT_DIR}/${artifact}" "${INSTALL_DIR}/${artifact}"
  done
fi

manifest_tmp="${INSTALL_MANIFEST}.tmp"
{
  printf '%s\n' \
    "${INSTALL_DIR}/codex-mp" \
    "${INSTALL_DIR}/codex-mp-uninstall"
  if [[ "${CODEX_MP_SKIP_PANEL:-0}" != "1" ]]; then
    printf '%s\n' "${INSTALL_DIR}/codex-mp-panel"
  fi
  if [[ "${install_patched_codex}" == "1" ]]; then
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

echo "installed ${INSTALL_DIR}/codex-mp"
echo "installed ${INSTALL_DIR}/codex-mp-uninstall"
if [[ "${CODEX_MP_SKIP_PANEL:-0}" == "1" ]]; then
  echo "panel build skipped (CODEX_MP_SKIP_PANEL=1)"
else
  echo "installed ${INSTALL_DIR}/codex-mp-panel"
fi
if [[ "${install_patched_codex}" == "1" ]]; then
  echo "installed patched Codex launchers in ${INSTALL_DIR}"
else
  echo "patched Codex build/install skipped (set CODEX_MP_BUILD_CODEX=1 or CODEX_MP_CODEX_ARTIFACT_DIR=...)"
fi
if [[ ":${PATH}:" != *":${INSTALL_DIR}:"* ]]; then
  echo "note: add ${INSTALL_DIR} to PATH before running codex-mp"
fi
