# Codex OmniBridge (模型切换与多模型桥接器)

> 让官方 Codex 能够自由切换第三方大模型（NewAPI、OneAPI、OpenRouter、本地大模型等），同时**绝不影响官方账号登录态**，保留原汁原味的官方订阅特权与生图等全部能力。

---

## 🌟 为什么需要 Codex OmniBridge？

在使用官方 Codex 时，你可能遇到过这些烦恼：
- 想用第三方的 DeepSeek、Gemini 3.8 Flash、Qwen、Claude 等模型，但切换配置后官方账号就退出了；
- 官方自带的生图、代码解释器或专属能力在切换后失效；
- 在同一个对话里，想中途换一个模型解答或补充，必须新建会话甚至改坏本地配置。

**Codex OmniBridge 正是为此而生：**
1. **官方模型路径保持不变**：stock Codex 只配置一个 `omnibridge` Responses provider，官方请求由 Router 原样透传到官方 backend，官方 Authorization、reasoning、hosted tools 与生图能力都不经过第三方兼容层；
2. **同对话切模型**：同一 Thread 的每轮 `model` 由 Router 按 registry 精确 route，可在官方和自定义模型之间切换；本地 stock app-server 的 official→custom→official fixture E2E 已通过，Remote Control 仍按合同单独验收；
3. **安全钥匙箱托管**：第三方 Provider 的 API Key 与托管账号的 OAuth 令牌都只存在系统钥匙串中，不写入 `providers.json` / `accounts.json` 等普通文件，也绝不随请求转发给第三方；
4. **内置嵌入式 Web 面板与远程访问**：单文件即可启动 Web 控制面板，用户可在网页端进行全量配置；支持密码保护及一键开启外网/局域网访问，实现多设备远程操控；
5. **M3 Expressive 界面**：面板按 Material 3 Expressive 规范构建——弹簧物理动效与形态变形、五档按钮尺寸、强调字阶、色调表面与自适应导航（窄屏底部导航栏 / 中屏导航轨道 / 宽屏抽屉），并内置浅色、深色、跟随系统三种主题与标准/高对比度、完整/减弱动效偏好。

> **关于 `auth.json` 的准确说明**：本项目的账号管理功能（Web 面板的「账号」页）会读取
> `~/.codex/auth.json` 以显示当前登录账号，并在你**主动点击切换账号**时备份并改写它。
> 除此之外，安装、同步、路由与卸载流程都不读写 `auth.json`；Profile 中的
> `model_provider` 由本项目管理，`auth.json` 的 OAuth 令牌永远不会被发送给任何第三方
> Provider。

---

## 🚀 快速上手

### 1. 安装与启动
- 从 [Releases 页面](../../releases) 下载适合你系统的 portable 包；
- Linux/macOS 使用 POSIX 安装脚本，Windows 使用 PowerShell 安装脚本；
- 全平台统一为单一纯命令行程序，无任何 GTK/WebKit 等复杂系统依赖。

启动 Web 控制面板进行管理：
```bash
# 启动本地 Web 面板（默认端口 31828）
codex-mp web start

# 为 Web 面板设置访问密码（设置后支持开启外网/局域网访问）
codex-mp web password "你的安全密码"

# 开启外网/局域网访问（绑定 0.0.0.0；必须先设置访问密码，否则会拒绝启动）
codex-mp web remote --enable
```
在浏览器中打开 `http://localhost:31828`（或远程设备打开 `http://<主机IP>:31828`）即可直观管理！

> **⚠️ 远程访问安全说明**
> 面板通过**明文 HTTP** 提供服务，没有 TLS。开启远程访问后，访问密码与所有请求内容
> 都会以明文经过网络，任何能读取该流量的人都可以接管面板。
> - 请**只**在可信内网中开启；开启时命令会再次打印安全警告。
> - 需要跨公网访问时，请保持远程访问**关闭**，改用隧道：
>   `ssh -L 31828:127.0.0.1:31828 <主机>`，然后在本地打开 `http://localhost:31828`。
> - 未设置密码时无法开启 0.0.0.0 绑定；登录接口带失败次数限流，连续失败会被临时拒绝。

### 面板界面资源（开发者）

面板是零依赖的本地 SPA，静态资源位于 `apps/panel/`，由 `crates/web` 通过
`RustEmbed` 内嵌、同时被 electron-builder 打包。两条分发路径共用同一份文件。

字体与图标均为**自托管**（`apps/panel/fonts/`）。这不是风格选择，而是必需：
Electron 以 `file://` 加载面板，离线时 CDN 字体不可用，且自托管才能让 CSP 的
`font-src` 保持 `'self'`、不引入任何第三方源。

| 文件 | 体积 | 覆盖 |
|---|---|---|
| `roboto-flex-latin*.woff2` | 143 KB | Roboto Flex 可变字体，Latin + Latin Ext |
| `noto-sans-sc.woff2` | 1020 KB | Noto Sans SC，GB2312 一级字库（3755 常用汉字） |
| `material-symbols-outlined.woff2` | 36 KB | 图标字形子集（原 3.9 MB） |

三者均保留 `wght` 可变轴，因此强调字阶（500/700）对中英文一并生效。
`Noto Sans SC` 按 `unicode-range` 限定在 CJK 区段，`Roboto Flex` 在前——所以
「Plus 主号」这类混排字符串的拉丁部分仍走 Roboto Flex，不会整串退化。

```bash
# 重新生成字体（需要 fonttools + brotli，且需要联网）
# 仅在图标集合或 CJK 覆盖范围变化时需要运行
./scripts/build-panel-fonts.sh

# 重新渲染品牌图标（assets/icon.svg -> assets/icon.png 与面板 favicon）
node scripts/build-icons.mjs

# 设计令牌纪律检查：语义层不得出现裸色值、基线字阶不得超过 500 字重、
# 必需的自托管字体与 Expressive 令牌必须存在
node scripts/check-panel-tokens.js
```

面板另有一套真实浏览器验证套件（结构 / 对比度 / 功能），见
`tools/panel-verify/README.md`：

```bash
npm i -D playwright && npx playwright install --with-deps chromium
node tools/panel-verify/run.mjs
```

> 新增图标时，请同时更新 `scripts/build-panel-fonts.sh` 中的 `ICONS` 列表并重跑，
> 然后在 `apps/panel/icons.css` 中使用生成的 `.m3-i-*` 类。图标以**码点类**而非连字名
> 寻址：连字会在上游重命名图标时静默失效（本轮重写就因此丢失过两个图标）。

### 2. 添加你的 AI 模型
无论在 Web 面板还是终端命令行，只需简单三步：
1. **添加服务商**：填入你的提供商名称、API 基础地址（Base URL）及 API 密钥；
2. **选择或添加模型**：自动抓取或手动指定模型名称；
3. **同步到 Codex**：一键同步生成兼容的 Codex catalog。

### 3. 在 Codex 中使用多模型
打开 Codex（无论在 CLI 还是客户端），进入你的对话：
- 在模型选择列表或输入 `/model` 时，你会发现你添加的模型（如 `newapi/gemini-3.8-flash`）已经赫然在列！
- 官方模型（如 GPT-6-Astra、GPT-5.6-Terra、GPT-5.6-Luna）依然直接走你的官方订阅；
- 选中自定义模型时，stock Codex 会将请求交给本地或远端 OmniBridge，再按 Provider 配置转发；本地受控 fixture 已验证 model/list、同 thread 三轮切换、compaction 和头/凭据隔离；真实官方网络、真实第三方网络和 Remote Control 仍是独立验收项。

### 4. Release 发布

发布规则和本地检查见 [Release 发布流程](docs/RELEASE.md)。GitHub Actions 的
`Release` workflow 默认创建 prerelease；只有选择正式通道并填写明确确认值时才会
创建正式版。每次发布必须填写更新说明，并自动校验 Windows、macOS 双架构、Linux
DEB 与 AppImage 安装包是否齐全。

---

## 🖥️ 命令行与无界面（Headless）使用

CLI / Router 核心现在按原生目标提供 Linux、macOS 和 Windows 构建。Windows
使用 .exe / .cmd 可执行文件解析和 PowerShell stock Codex 构建入口；macOS 使用
原生 Rust 进程管理，不需要 Bash 的 /dev/tcp 扩展。WSL2 仍视为 Linux 环境，
不会替代原生 Windows CLI。

Windows PowerShell：

```powershell
cargo build --release --locked --package codex-mp-cli
$env:CODEX_MP_MANAGER_BIN = (Resolve-Path .\target\release\codex-mp.exe).Path
& .\target\release\codex-mp.exe --codex-bin (Get-Command codex).Source sync
& .\target\release\codex-mp.exe launch --codex-binary (Get-Command codex).Source -- exec
```

如果需要实验性的 Desktop runtime adapter，再额外构建与固定上游 commit
匹配的 patched Codex artifact：

```powershell
.\scripts\build-patched-codex.ps1 -Output .\dist\stock-codex
```

安装 Windows CLI/可选 Desktop 适配：

```powershell
$env:CODEX_MP_CODEX_ARTIFACT_DIR = "$PWD\dist\stock-codex"
$env:CODEX_MP_INSTALL_DESKTOP = "1"
powershell -ExecutionPolicy Bypass -File .\installer\install-windows.ps1
```

macOS/Linux：

```bash
cargo build --release --locked --package codex-mp-cli
./target/release/codex-mp --codex-bin "$(command -v codex)" sync
./target/release/codex-mp launch --codex-binary "$(command -v codex)" -- exec
```

如需可搬运的 legacy/experimental Desktop artifact，再执行
`./scripts/build-patched-codex.sh --output ./dist/stock-codex`，并通过
`CODEX_MP_MANAGER_BIN`/`PATH` 让同目录 launcher 找到 `codex-mp` manager。

macOS 安装 CLI/可选 Desktop 适配：

```bash
CODEX_MP_CODEX_ARTIFACT_DIR="$PWD/dist/stock-codex" \
CODEX_MP_INSTALL_DESKTOP=1 ./installer/install-macos.sh
```

启动器会复用同一用户目录下已有的健康 Router；如果没有，则启动 loopback
Router。默认构建的是 stock Codex，官方 codex 可执行文件不会被覆盖。

如果你在没有图形界面的 Linux 服务器或更喜欢用终端：
```bash
# 1. 安全添加服务商与 API Key（支持从管道安全传入，不在终端历史留痕）
export NEWAPI_KEY='你的密钥'
printf '%s' "$NEWAPI_KEY" | codex-mp provider add NewAPI https://api.example.com/v1 --api-key-stdin

# 2. 导入模型
codex-mp provider fetch-models newapi --add qwen3.8

# 3. 同步至 Codex
codex-mp sync

# 4. 启动（或安装）Router —— resume 与自定义模型都需要它先运行
codex-mp launch                      # 随 Codex 启动一个临时 Router
# 或：安装远端常驻 Router（仅安装 unit；按需设置 CODEX_MP_ENABLE_SERVICE=1 启动）
codex-mp-router-service-install

# 5. 旧 thread 的显式迁移（不会改写 rollout/history 文件）
#    前置条件：上一步的 Router 必须已经在 config.toml 指定的端口上监听，
#    否则 `resume` 会直接报错并提示如何启动（不会静默连到空端口）。
codex-mp resume --through-omnibridge <SESSION_ID>
```

Linux 远端常驻 Router：先在远端完成 `codex-mp sync`，再执行
`codex-mp-router-service-install`。服务使用稳定 loopback 端口和 registry
派生的 capability/config，不读写 `auth.json`。Remote Control 必须连接
这台远端 stock app-server；本地 Desktop 不安装旧 Core patch。

`installer/install-router-service.sh` 与
`installer/uninstall-router-service.sh` 只管理 systemd user unit，不删除
provider registry 或 keyring 凭据。

旧 thread 不会因为 catalog 更新而静默改变 provider。`resume --through-omnibridge`
只向 stock Codex 的 resume 请求显式传入 `model_provider="omnibridge"`；如果旧
thread 的历史无法安全跨 provider 复用，应按命令提示 fork 一个新 thread。

## 🖥️ ChatGPT Desktop 兼容边界

Remote Control 的默认路径不替换本地 Desktop runtime：在远端安装 stock
app-server、OmniBridge catalog 和常驻 Router，本地 Desktop 只连接远端。下面的
Desktop runtime adapter 是保留的 legacy/experimental 兼容路径，不是 Remote
Control 的完成证明，也不属于默认 stock 构建路径。

只有明确需要实验性本地 adapter 时，才先准备它要求的历史 patched artifact，
完全退出 ChatGPT Desktop，再执行：

```bash
codex-mp desktop status
codex-mp desktop install \
  --app-server-binary ~/.local/bin/codex-mp-app-server-bin \
  --codex-mp-binary ~/.local/bin/codex-mp
```

Linux 会把用户级 standalone `codex` 入口替换为受管 POSIX launcher；Windows 和
macOS 不改写官方 Desktop runtime，而是创建用户级原生 launcher，并通过
`CODEX_CLI_PATH` 让官方 Desktop 调用它。Windows 的覆盖写入当前用户的
`HKCU\Environment`，macOS 写入当前用户的 `launchctl` 环境。三种平台都将
stock app-server 与 Desktop 自带的 `codex-code-mode-host` 放入独立 runtime，
并在 registry 所在目录保存哈希、备份和 manifest。

这个过程不修改 OAuth `auth.json`、Thread/历史数据库、官方模型身份或官方
Desktop 安装包（Linux 只改用户级 standalone 入口）。安装完成后完全退出并重新
打开 ChatGPT Desktop；Windows/macOS 只在适配器处于 managed 状态时通过
`CODEX_CLI_PATH` 选择本项目 launcher，恢复后会清除该覆盖并回到官方入口。

恢复官方 Desktop：

```bash
codex-mp desktop restore
```

Windows PowerShell 和 macOS/Linux 的完整安装器分别是
`installer/install-windows.ps1` 与 `installer/install-macos.sh`；设置
`CODEX_MP_INSTALL_DESKTOP=1` 仅用于上述 legacy/experimental adapter；Remote
Control 不需要它。若只需 stock CLI 或远端 app-server，不设置该变量。

Desktop picker、真实 Account、真实第三方 Provider E2E，以及 Desktop 自动更新后的
重新适配仍必须在目标机逐项验收；本地 stock app-server 的同 Thread 官方→自定义→
官方受控 fixture 已通过，但源码、单元测试和安装成功本身不等于 Remote Control
产品级能力已经被证明。

---

## 🛡️ 卸载与清理（零污染承诺）

如果你不再需要本程序，可以在设置中点击卸载，或在终端执行：
```bash
codex-mp-uninstall
```
程序会尝试：
- 停止后台路由器；
- 恢复你最初的 `config.toml` 配置；
- 清理本地注册表与密钥；
- **保留 ChatGPT 登录状态与官方历史存储；如果检测到用户手动改动受管字段，会拒绝覆盖并报告错误。**

---

## 📋 常见问题 (FAQ)

**Q：使用自定义模型会消耗我的官方额度吗？**  
A：不会。请求官方模型时走你的官方账号；请求自定义模型时仅消耗你在对应服务商（如 NewAPI / OpenRouter）里的额度。

**Q：官方会不会收到第三方 Provider 的 API Key？**
A：设计上不会。API Key 只由本地 Router 读取，stock Codex 仅携带独立的 Router capability header；真实 Provider E2E 仍需使用实际账号单独验收。

---

## 📄 开源许可证

本项目基于 [MIT 许可证](LICENSE) 开源发布。
