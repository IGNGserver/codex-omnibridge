#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

DEFAULT_VERSION="$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "${PROJECT_DIR}/package.json" | head -n 1)"
VERSION="${1:-${DEFAULT_VERSION}}"
PACKAGE_VERSION="${VERSION#v}"
ARCH="${2:-amd64}"
PKG_NAME="codex-omnibridge_${PACKAGE_VERSION}_${ARCH}"
BUILD_DIR="${PROJECT_DIR}/dist/deb-build/${PKG_NAME}"
OUTPUT_DIR="${PROJECT_DIR}/dist"

if [[ ! "${PACKAGE_VERSION}" =~ ^[0-9] ]]; then
  echo "error: Debian package version must start with a digit: ${VERSION}" >&2
  exit 2
fi

echo "==> 构建 Linux DEB 安装包: ${PKG_NAME}.deb"
mkdir -p "${BUILD_DIR}/DEBIAN"
mkdir -p "${BUILD_DIR}/usr/bin"
mkdir -p "${BUILD_DIR}/usr/share/applications"
mkdir -p "${BUILD_DIR}/usr/share/icons/hicolor/512x512/apps"
mkdir -p "${OUTPUT_DIR}"

# 1. 拷贝程序文件与图标
cp "${PROJECT_DIR}/target/release/codex-mp" "${BUILD_DIR}/usr/bin/codex-mp"
chmod 0755 "${BUILD_DIR}/usr/bin/codex-mp"

# Remove the previous vector icon when reusing a packaging directory.
rm -f "${BUILD_DIR}/usr/share/icons/hicolor/scalable/apps/codex-omnibridge.svg"
cp "${PROJECT_DIR}/assets/icon.png" "${BUILD_DIR}/usr/share/icons/hicolor/512x512/apps/codex-omnibridge.png"

# 2. 生成 desktop 快捷方式
cat >"${BUILD_DIR}/usr/share/applications/codex-omnibridge.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Codex OmniBridge
Comment=Codex 模型切换与多模型桥接器 Web 面板
Exec=/usr/bin/codex-mp web start --open
Icon=codex-omnibridge
Terminal=false
Categories=Development;
StartupNotify=false
EOF
chmod 0644 "${BUILD_DIR}/usr/share/applications/codex-omnibridge.desktop"

# 3. 生成 DEBIAN/control
cat >"${BUILD_DIR}/DEBIAN/control" <<EOF
Package: codex-omnibridge
Version: ${PACKAGE_VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Maintainer: Codex MultiProvider Contributors
Description: Codex OmniBridge - Seamless model switching & Web management panel for Codex.
 Completely headless-compatible single binary with zero GTK dependencies.
EOF

# 4. 安装 DEBIAN/prerm (卸载前停止运行中的进程，安全还原受管配置)
#
# 使用 installer/linux/deb/prerm 这一份**唯一**实现，不再内联生成第二份。
# 内联副本与仓库版本已经分叉：它用 `pkill -f '/usr/bin/codex-mp'`（子串匹配，
# 会误伤命令行里含该路径的无关进程），且只在 SUDO_USER 存在时才调用 uninstall
# —— 以 root 身份直接卸载（容器、CI、`su`）时什么都不做，用户配置不会被还原。
PRERM_SOURCE="${SCRIPT_DIR}/../installer/linux/deb/prerm"
if [[ ! -f "${PRERM_SOURCE}" ]]; then
  PRERM_SOURCE="${PROJECT_DIR}/installer/linux/deb/prerm"
fi
if [[ ! -f "${PRERM_SOURCE}" ]]; then
  echo "error: canonical deb prerm not found (expected installer/linux/deb/prerm)" >&2
  exit 2
fi
install -m 0755 "${PRERM_SOURCE}" "${BUILD_DIR}/DEBIAN/prerm"

# 5. 生成 DEBIAN/postrm (清理残留)
cat >"${BUILD_DIR}/DEBIAN/postrm" <<'EOF'
#!/usr/bin/env bash
set -e
if [[ "$1" = "purge" ]]; then
  echo "Codex OmniBridge purged completely."
fi
exit 0
EOF
chmod 0755 "${BUILD_DIR}/DEBIAN/postrm"

# 6. 打包
dpkg-deb --root-owner-group --build "${BUILD_DIR}" "${OUTPUT_DIR}/${PKG_NAME}.deb"
echo "==> DEB 包制作成功: ${OUTPUT_DIR}/${PKG_NAME}.deb"
