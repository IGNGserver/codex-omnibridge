# Codex OmniBridge Official Website (`website/`)

Codex OmniBridge 官方网站源码。采用 **Precision Photonics Laboratory × Material 3 Expressive Instrument** 视觉与交互体系构建，100% 自托管可变字体与 3D 光学硬件资产，零外部 CDN 运行时请求。

## 核心特性

- **参数化精密 3D 光学交换矩阵 (`public/models/omnibridge-photonic-matrix.glb`)**：由 `scripts/build-photonic-glb.mjs` 生成的 17 节点、32,884 三角面二进制 glTF 2.0 硬件资产，包含可开合高硼硅石英防尘罩（`MeshPhysicalMaterial` 折射透射）、目录合成双编码轮、双轴 MEMS 振镜分流十字架、三联布儒斯特协议色散棱镜、法拉第系统钥匙箱与铜绕本机回环谐振腔。
- **自定义 GLSL 光孤子波导着色器 (`src/scene/OpticalWaveguides.ts`)**：当用户在 GPT（官方直通 Channel α）、Claude / Qwen / DeepSeek / Gemini（协议棱镜桥接 Channel β）与 Ollama（本机零外发回环 Channel γ）间切换时，3D 振镜实时执行阻尼弹簧偏转并重连脉冲光路。
- **滚动驱动内部光机巡检 (`#inside-matrix`)**：随滚动自动掀开石英机盖并推进镜头，逐级展示 `crates/catalog`、`crates/router`、`crates/protocol-bridge` 与 `crates/credentials` 的真实架构细节。
- **同会话四轮热切换推演台 (`src/ui/ThreadSimulator.ts`)**：直观演示同一 `thread_id` 内连续执行 `/model` 切换时的请求头白名单放行、跨源凭据剥离与 `CodexChatHistoryStore` 历史水合。
- **可交互 M3 Expressive 控制中心 (`src/ui/PanelSandbox.ts`)**：忠实复刻 `#00A6B8` CIELAB 色调调色板、CSS `linear()` 阻尼弹簧曲线、浅色/深色与高对比度切换、服务商自动发现、非环回强制 HTTPS 校验与 `models.json` 实时同步预览。

## 本地开发与验证（遵循 `BUILD_SCRATCH_ROOT` 规范）

为避免在项目目录产生 `node_modules/` 或 `dist/`，建议将源码同步至本机盘构建区（`${BUILD_SCRATCH_ROOT:-$HOME/.build}`）执行构建与验证：

```bash
SCRATCH="${BUILD_SCRATCH_ROOT:-$HOME/.build}/codex模型切换器/website-build"
mkdir -p "$SCRATCH"
rsync -a --delete --exclude node_modules --exclude dist ./ "$SCRATCH/"

cd "$SCRATCH"
pnpm install
pnpm run generate:glb   # 可选：重新生成 public/models/omnibridge-photonic-matrix.glb
pnpm run build          # tsc --noEmit && vite build
pnpm run verify         # 启动本地静态服务并执行 Playwright 桌面+移动端全链路与 WebGL 校验
```
