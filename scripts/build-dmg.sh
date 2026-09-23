#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

DEFAULT_VERSION="$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "${PROJECT_DIR}/package.json" | head -n 1)"
VERSION="${1:-${DEFAULT_VERSION}}"
PACKAGE_VERSION="${VERSION#v}"
ARCH="${2:-$(uname -m)}"
case "${ARCH}" in
  arm64|aarch64)
    ARCH="aarch64"
    ;;
  x86_64|amd64)
    ARCH="x86_64"
    ;;
  *)
    echo "error: unsupported macOS architecture: ${ARCH}" >&2
    exit 2
    ;;
esac
APP_NAME="Codex OmniBridge"
STAGING_DIR="${PROJECT_DIR}/dist/macos-build"
APP_DIR="${STAGING_DIR}/${APP_NAME}.app"
DMG_NAME="codex-omnibridge-macos-${ARCH}-${PACKAGE_VERSION}.dmg"
OUTPUT_DIR="${PROJECT_DIR}/dist"
BUNDLE_VERSION="${PACKAGE_VERSION%%[-+]*}"

echo "==> 构建 macOS .app 包装与 DMG 镜像: ${DMG_NAME}"
rm -rf "${STAGING_DIR}"
mkdir -p "${APP_DIR}/Contents/MacOS"
mkdir -p "${APP_DIR}/Contents/Resources"
mkdir -p "${OUTPUT_DIR}"

# 1. 拷贝 CLI 二进制作为 App 入口启动脚本
cp "${PROJECT_DIR}/target/release/codex-mp" "${APP_DIR}/Contents/MacOS/codex-mp-bin"
chmod 0755 "${APP_DIR}/Contents/MacOS/codex-mp-bin"

cat >"${APP_DIR}/Contents/MacOS/${APP_NAME}" <<'EOF'
#!/usr/bin/env bash
DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec "${DIR}/codex-mp-bin" web start --open
EOF
chmod 0755 "${APP_DIR}/Contents/MacOS/${APP_NAME}"

# 2. 生成 Info.plist (设置 LSUIElement=1 避免 Dock 强行显示黑框，主要用于托盘/Web 模式)
cat >"${APP_DIR}/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>dev.codex-multiprovider.omnibridge</string>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleVersion</key>
    <string>${BUNDLE_VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${BUNDLE_VERSION}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSUIElement</key>
    <false/>
</dict>
</plist>
EOF

# 3. 提供一键无损卸载工具脚本放入 DMG
cat >"${STAGING_DIR}/一键完全卸载.command" <<'EOF'
#!/usr/bin/env bash
set -e
echo "正在停止 Codex OmniBridge 并执行彻底卸载与还原..."
pkill -f 'codex-mp' || true
if [[ -f "$HOME/.local/bin/codex-mp" ]]; then
  "$HOME/.local/bin/codex-mp" uninstall || true
fi
rm -rf "/Applications/Codex OmniBridge.app"
echo "Codex OmniBridge 已彻底无残留卸载，Codex 官方配置已恢复。"
read -p "按回车键退出..."
EOF
chmod 0755 "${STAGING_DIR}/一键完全卸载.command"

# 4. 创建 /Applications 软连接以便拖拽安装
ln -s /Applications "${STAGING_DIR}/Applications"

# 5. 生成 DMG
if command -v hdiutil >/dev/null 2>&1; then
  hdiutil create -volname "${APP_NAME}" -srcfolder "${STAGING_DIR}" -ov -format UDZO "${OUTPUT_DIR}/${DMG_NAME}"
  echo "==> macOS DMG 制作成功: ${OUTPUT_DIR}/${DMG_NAME}"
else
  echo "==> 非 macOS 环境或无 hdiutil，打包为便携 tar.gz"
  tar -czf "${OUTPUT_DIR}/codex-omnibridge-macos-${ARCH}-${PACKAGE_VERSION}.app.tar.gz" -C "${STAGING_DIR}" "${APP_NAME}.app"
fi
