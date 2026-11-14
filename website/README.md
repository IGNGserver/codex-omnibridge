# Codex OmniBridge 官网展示前端

`website/` 目录承载 Codex OmniBridge 的独立静态官网（托管域名 `https://omnibridge.igng.net/`）。

## 设计与技术要点

- **Material 3 Expressive 浅色默认 + 深色切换**：配色、间距、圆角、阴影与交互组件对齐应用本体控制面板（主色 `#00A6B8` / `#006874`），支持随系统偏好自动切换或手动一键切换。
- **交互式 3D 分流模型（`three` + 离线生成的 `.glb`）**：开放式三路路由器模型展示“官方直连 / 协议转换+钥匙串 / 本机模型”分流轨迹，禁用易起雾的后处理泛光并限制透明外壳防发白；WebGL 不可用时自动降级到主题化矢量视图。
- **控制面板沙盒 & 多轮混用演练**：无需安装即可试用服务商 HTTPS 校验、一键同步 `/model` 列表及同一条会话切换 GPT / Claude / DeepSeek / Ollama 时的凭据隔离过程。
- **真实发版直链与零外链字体**：提供 `v0.1.2` 全平台 GitHub Releases 下载与 `SHA256SUMS` 链接，自托管 Noto Sans SC / Roboto Flex / Material Symbols 字体，不发起第三方 CDN 请求。

## 开发与验证

注意：按照仓库规范，依赖安装（`node_modules`）与构建输出（`dist`）应放在本机盘 `$BUILD_SCRATCH_ROOT` 下运行，不要将临时构建产物留在源码目录。

```sh
# 1. 重新生成交互式 3D 模型（写入 public/models/omnibridge-photonic-matrix.glb）
pnpm run generate:glb

# 2. 类型检查并打包（支持通过 SITE_URL 指定规范域名）
SITE_URL=https://omnibridge.igng.net/ pnpm run build

# 3. 无头浏览器全链路验证（3D 渲染、交互沙盒、下载链接、多端零横向溢出、无外链请求）
pnpm run verify
```
