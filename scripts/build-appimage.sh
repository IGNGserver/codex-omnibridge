#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

DEFAULT_VERSION="$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "${PROJECT_DIR}/package.json" | head -n 1)"
VERSION="${1:-${DEFAULT_VERSION}}"
PACKAGE_VERSION="${VERSION#v}"
ARCH="${2:-x86_64}"
if [[ "${ARCH}" != "x86_64" ]]; then
  echo "error: Linux AppImage builder currently supports x86_64 only: ${ARCH}" >&2
  exit 2
fi

CLI_BINARY="${PROJECT_DIR}/target/release/codex-mp"
APPDIR="${PROJECT_DIR}/dist/appimage-build/codex-omnibridge.AppDir"
OUTPUT_DIR="${PROJECT_DIR}/dist"
OUTPUT="${OUTPUT_DIR}/codex-omnibridge-linux-${ARCH}-${PACKAGE_VERSION}.AppImage"
APPIMAGE_TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"

[[ -x "${CLI_BINARY}" ]] || {
  echo "error: release binary is missing or not executable: ${CLI_BINARY}" >&2
  exit 1
}
[[ -n "${APPIMAGE_TOOL}" && -x "${APPIMAGE_TOOL}" ]] || {
  echo "error: appimagetool is required; set APPIMAGETOOL or install it on PATH" >&2
  exit 1
}

echo "==> 构建 Linux AppImage: $(basename "${OUTPUT}")"
rm -rf "${APPDIR}"
mkdir -p \
  "${APPDIR}/usr/bin" \
  "${APPDIR}/usr/share/applications" \
  "${APPDIR}/usr/share/icons/hicolor/512x512/apps" \
  "${OUTPUT_DIR}"

install -m 0755 "${CLI_BINARY}" "${APPDIR}/usr/bin/codex-mp"
install -m 0644 "${PROJECT_DIR}/assets/icon.png" \
  "${APPDIR}/usr/share/icons/hicolor/512x512/apps/codex-omnibridge.png"
install -m 0644 "${PROJECT_DIR}/assets/icon.png" "${APPDIR}/codex-omnibridge.png"

cat >"${APPDIR}/AppRun" <<'EOF'
#!/usr/bin/env sh
set -eu
HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
if [ "$#" -eq 0 ]; then
  exec "${HERE}/usr/bin/codex-mp" web start --open
fi
exec "${HERE}/usr/bin/codex-mp" "$@"
EOF
chmod 0755 "${APPDIR}/AppRun"

cat >"${APPDIR}/codex-omnibridge.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Codex OmniBridge
Comment=Codex model switching and multi-provider bridge
Exec=codex-omnibridge
Icon=codex-omnibridge
Terminal=false
Categories=Development;
StartupNotify=false
EOF

rm -f "${OUTPUT}"
ARCH="${ARCH}" APPIMAGE_EXTRACT_AND_RUN=1 \
  "${APPIMAGE_TOOL}" "${APPDIR}" "${OUTPUT}"
[[ -s "${OUTPUT}" ]] || {
  echo "error: appimagetool did not produce ${OUTPUT}" >&2
  exit 1
}
echo "==> AppImage 制作成功: ${OUTPUT}"
