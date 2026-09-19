# Codex OmniBridge 面板 M3 Expressive 改造方案（全量改动清单）

> 文档状态：**设计改造方案 + 全量改动清单**（✅ 已实施完毕，见
> `docs/M3_EXPRESSIVE_UI_IMPLEMENTATION_REPORT.md` 的实施与验证报告）
> 编写日期：2026-09-18
> 目标基线：工作区 `416f3bd`
> 改造对象：`apps/panel/`（内嵌 Web 控制面板）、`apps/electron/`（桌面外壳）
> 关联文档：`docs/VERIFICATION_AND_FIX_PLAN.md`、`docs/CODE_AUDIT_AND_FIX_PLAN.md`
> 规范出处：[Material 3 Expressive](https://m3.material.io/blog/building-with-m3-expressive)、
> [Motion specs](https://m3.material.io/styles/motion/overview/specs)、
> [Shape morph](https://m3.material.io/styles/shape/shape-morph)、
> [Buttons](https://m3.material.io/components/buttons/overview)、
> [Loading indicator](https://m3.material.io/components/loading-indicator/overview)、
> [Progress indicators](https://m3.material.io/components/progress-indicators/overview)

---

## 0. 本文档的定位

现面板（`apps/panel/`）已经是一套**手写的 M3 基线实现**：有 `--md-sys-*` 令牌层、有
Material Symbols、有 Filled/Tonal/Outlined/Text 按钮、有 dialog/snackbar/switch/progress。
但它是**按 M3 基线规范（2021 版）写的，不是 M3 Expressive**。差距集中在六处：

| # | 现状特征 | M3 Expressive 要求 |
|---|---|---|
| 1 | 单一 40px 按钮高度 | 五档按钮尺寸 XS/S/M/L/XL = 32/40/56/96/136 |
| 2 | 圆角只有 0/4/8/12/16/28/full | 补 20（large-increased）/32（XL-increased）/48（XXL） |
| 3 | 动效只有 easing + 固定 duration，且 `--md-motion-easing-emphasized` 被错误地写成与 standard 同值 | 以**弹簧物理**为主（三组 spring 令牌），easing 仅用于转场 |
| 4 | 字重 600/700 满天飞（`grep` 到 16 处） | 基线只有 400/500；强调靠 **15 个 emphasized 角色**（400→500，500→700），且**字号行高不变** |
| 5 | 深度靠 `box-shadow`（4 级 elevation 全是阴影） | 深度靠**色调表面**（tonal surface），阴影只作补充 |
| 6 | 响应式只有 `@media (max-width: 900px)` 一个断点 | 按 **Window size class**（compact/medium/expanded/large/extra-large）分档 |

本文档把「从 M3 基线 → M3 Expressive」需要改的**每一处**列出来，按用户指定的六个
维度组织，最后再按**文件**汇总成可执行的清单。

### 0.1 一个必须写在前面的技术事实

**M3 Expressive 没有官方 Web 实现。** [`@material/web` 已进入维护模式，M3 Expressive
不在 Web 上提供](https://m3.material.io/develop/web)，Google 官方的 Expressive
组件（FAB menu、split button、button groups、floating toolbar、loading indicator、
wavy progress）目前只有 Compose / MDC-Android 实现。本项目面板是 **CSP 收紧的
本地 SPA（`script-src 'self'`，无内联脚本、无构建步骤）**，不能也不应引入 Compose。

因此本方案的技术路线是固定的：

> **把手写的 CSS/JS 组件层升级到 Expressive 的令牌与形态规范**，用 CSS 变量表达
> spring 运动（`linear()` 缓动或 `@keyframes` + 阻尼采样），用原生 JS 实现形态
> 与尺寸变体。**不引入任何新依赖**（`package.json` 目前只有 electron 相关 devDeps，
> 面板是零依赖的）。

这条路线同时意味着：**Expressive 的"形"可以完整做到，Expressive 的"物理精度"
只能逼近**。第 12 节列出这一点带来的风险与取舍。

---

## 1. 现状基线（实测）

### 1.1 文件与职责

| 文件 | 行数 | 职责 | 是否要改 |
|---|---|---|---|
| `apps/panel/index.html` | 526 | 静态骨架、4 个视图、6 个对话框、CSP、图标库引入 | ✅ 大改 |
| `apps/panel/styles.css` | 1039 | 全部设计令牌 + 全部组件样式 | ✅ 大改 |
| `apps/panel/main.js` | 1372 | 路由、API、渲染（大量 `innerHTML` 模板字符串） | ✅ 中改 |
| `apps/electron/main.js` | 434 | 无边框窗口、尺寸、背景色、托盘 | ✅ 小改 |
| `apps/electron/preload.js` | 21 | 暴露 `electronAPI`（token / apiBase / 窗口控制） | ⚠️ 可能小改 |
| `crates/web/src/lib.rs` | 2191 | `RustEmbed` 内嵌 `apps/panel`、CSP 响应头、静态托管 | ✅ 小改 |
| `assets/icon.svg` | 8 | 应用图标（硬编码调色板 + `rx=16`） | ✅ 小改 |
| `.github/workflows/ci.yml` | — | `node --check`、CSP 存在性、`webSecurity` 检查 | ✅ 追加检查 |
| `README.md` | — | 面板使用说明 | ⚠️ 视改动幅度 |

**构建链路（决定了改动落到哪里）：**

```
apps/panel/{index.html,styles.css,main.js}
   ├─ RustEmbed(folder="../../apps/panel")  →  crates/web 内嵌，HTTP 面板(31828)
   └─ electron-builder files: apps/panel/** →  Electron 本地 file:// 加载
```

→ **面板是"一次编写、两条分发路径"**。新增静态资源（字体、图标 sprite）必须同时
满足：`RustEmbed` 能取到（放在 `apps/panel/` 下即可）且 `mime_guess` 能给出正确
Content-Type（`woff2` 已支持，实测 `font/woff2`），以及 electron-builder 的
`files` glob 覆盖到（`apps/panel/**/*` 已覆盖）。

→ `dist/electron/**`、`dist/*.AppImage`、`dist/*.deb` 是**构建产物**，不得手改。

### 1.2 现有设计令牌盘点（`styles.css` L1–L63）

**颜色角色（浅色 L3–L40 / 深色 L66–L103）：**

已有：`primary`、`on-primary`、`primary-container`、`on-primary-container`、
`secondary`、`on-secondary`、`secondary-container`、`on-secondary-container`、
`tertiary`、`on-tertiary`、`tertiary-container`、`on-tertiary-container`、
`error`、`on-error`、`error-container`、`on-error-container`、`background`、
`on-background`、`surface`、`on-surface`、`surface-variant`、`on-surface-variant`、
`surface-container-lowest/low//high/highest`、`outline`、`outline-variant`、`scrim`。

**缺失（Expressive 必需）：** `surface-dim`、`surface-bright`、`surface-tint`、
`inverse-surface`、`inverse-on-surface`、`inverse-primary`、`shadow`、
`primary-fixed`、`primary-fixed-dim`、`on-primary-fixed`、`on-primary-fixed-variant`、
`secondary-fixed`(×4)、`tertiary-fixed`(×4)、`surface-container-*` 的
`on-*` 配对语义、以及高对比模式下的 `on-surface-variant` 修正。

**形状令牌（L49–L55）：** `none/small(8)/medium(12)/large(16)/extra-large(28)/full`
→ 缺 `extra-small(4)` 已被声明但 **`large-increased(20)`、`extra-large-increased(32)`、
`extra-extra-large(48)`** 三个 Expressive 档位不存在。

**运动令牌（L58–L62）：** 只有 3 个 duration（150/250/350ms）+ 2 个 easing。
其中 **`--md-motion-easing-emphasized: cubic-bezier(0.2, 0, 0, 1)` 与 standard 完全相
同**，即"强调曲线"实际不存在；`--md-motion-duration-long` 声明了但全文件未使用。

**高度令牌（L42–L46 / L105–L108）：** 4 级全部是 `box-shadow`，与 M3「色调表面为
主、阴影为辅」的原则相反；且深色模式下阴影 alpha 高达 0.35–0.65。

### 1.3 现有硬编码颜色与反模式（实测清单）

| 位置 | 值 | 问题 |
|---|---|---|
| `styles.css` L145/150/154 | `rgba(255,255,255,.16/.3/.45)` | 滚动条只在深色下成立，浅色主题下不可见 |
| `styles.css` L254/258 | `rgba(255,255,255,.08/.14)` | 窗口按钮 hover/active 同上 |
| `styles.css` L262/263/267/268 | `#e81123` / `#bf0f1d` | 关闭按钮用 Windows 品牌红，非 M3 角色（应为 `error`） |
| `styles.css` L281 | `linear-gradient(135deg, primary, tertiary)` | 典型 "AI 渐变" 反模式，且无法随主题对比度调整 |
| `styles.css` L349/475/486/517/588/590/821 | `rgba(130,213,229,…)` | **把深色主题的 primary 硬编码进语义层**，浅色主题下 state layer 变色 |
| `styles.css` L464 | `rgba(177,203,207,.28)` | tonal hover 同上 |
| `styles.css` L594/595/596 | `#ffc777` / `rgba(255,199,119,…)` | 警告色不在令牌体系内，深色主题专用 |
| `styles.css` L614–627 | `#1a434d/#82d5e5/#3b2a59/#d0bcff/#2e432f/#a7e2af` | plus/pro/team 三个 plan badge 的底色与文字色全硬编码（共 6 个色值，`.free`/`.unknown` 已令牌化），静默失效于浅色主题 |
| `styles.css` L669 | `#e5a93b` | 进度条 warning 同上 |
| `styles.css` L928 | `rgba(0,0,0,.65)` | scrim 未使用 `--md-sys-color-scrim` |
| `electron/main.js` L211 | `backgroundColor: "#0f1416"` | 硬编码深色表面，浅色主题下启动白/深色闪屏 |
| `assets/icon.svg` | `#111827/#22d3ee/#f59e0b` | 品牌色与面板 primary 无关联 |
| `styles.css` L54 | `9999px` | 魔法半径（M3 用 `full` 语义，但代码里也是裸 9999px，见 L429 等） |
| `main.js` L657/658 | `rgba(130,213,229,.12)` / `rgba(218,226,255,.18)` | 模型能力 badge 行内硬编码 |

### 1.4 现有组件清单（`styles.css` → 使用处）

| 组件 | CSS 位置 | HTML/JS 使用 | 是否 Expressive 缺口 |
|---|---|---|---|
| `.m3-btn`（+filled/tonal/outlined/text/danger） | L422–499 | 26 处 | ⚠️ 仅 1 档高度（40px），无 XS/L/XL，无状态层，无按压变形 |
| `.m3-icon-btn` | L502–519 | `index.html` L45/49，`main.js` L666/669/865/868 | ⚠️ 40px，无形态变形，无 4 种变体（standard/filled/filled-tonal/outlined） |
| `.m3-fab` | L522–546 | **0 处使用（死代码）** | ❌ 未出现；Expressive 主推 FAB menu |
| `.m3-card`（+elevated/filled/outlined） | L549–573 | 11 处 | ⚠️ 统一 28px 圆角（应 12/16），高度靠 shadow |
| `.m3-badge`（+success/warning/error） | L576–602 | 多处 | ⚠️ 与 M3 Badge 定义不同：M3 badge 是 6dp dot / 16dp 计数，不是状态胶囊 |
| `.m3-plan-badge` | L604–638 | `main.js` L533 | ⚠️ 硬编码色 |
| `.m3-progress-*` | L641–674 | `main.js` L275/295/313/333 | ⚠️ 8px 直线进度条；Expressive 提供 wavy 形 |
| `.m3-switch` | L677–727 | `index.html` L298/309，`main.js` L662 | ⚠️ 结构对，但无图标、无按压回弹、无 48dp 触摸区 |
| `.m3-text-field` / `.m3-input` / `.m3-select` / `.m3-textarea` | L730–757 | 13+11+1+1 处 | ❌ 不是 M3 做法：M3 用 Filled/Outlined **带浮动标签**；这里是"label 在上面 + 输入框在下面"的自造结构，且无 supporting text / 字符计数 / 错误态 / 前后置图标 |
| `.m3-dialog` / `.m3-dialog-backdrop` | L922–977 | 6 个对话框 | ❌ 用 `<div>` + `style.display` 手写，**无 `role="dialog"`、无 `aria-modal`、无焦点陷阱、无 ESC 关闭、无滚动锁定** |
| `.m3-snackbar` | L982–1013 | `main.js` L88–102 | ⚠️ 应为 `inverse-surface`；无 action 按钮、无排队、无 aria-live |
| `.m3-navigation-drawer` / `.m3-nav-item` | L319–373 | `index.html` L70–94 | ⚠️ 固定 256px；M3 Expressive **弱化了 phone 上的 navigation drawer，转向 flexible navigation bar**；本项目是桌面应用，应由 drawer → **navigation rail → drawer+rail 自适应** |
| `.m3-top-app-bar` | L202–215 | `index.html` L25 | ⚠️ 48px（M3 small app bar 为 64px）；无 scrolled 状态；无 medium/large 变体 |
| `.m3-metric-card` | L764–801 | `index.html` L114/125/136/147 | ⚠️ 静态卡片，可升级为 Expressive 大数字 + emphasized 排版 |
| `.m3-account-card` | L804–859 | `main.js` L520 | ⚠️ |
| `.m3-provider-card` / `.m3-model-row` | L862–909 | `main.js` L844/646 | ⚠️ |
| 空状态 | `index.html` L229–232/260–263，`main.js` L507–513/970–976 | — | ⚠️ 是"图标+文字"裸空态，非 Expressive 的 composed empty state |
| **缺失**：loading indicator | — | — | ❌ Expressive 要求用形态变形的 loading indicator 取代不定量圆形进度 |
| **缺失**：FAB menu / split button / button group / toolbar | — | — | ❌ 面板里完全没有 |
| **缺失**：list item / divider / tooltip / segmented button | — | — | ❌ 现有列表用 `div` 拼，tooltip 只有 14 个 `title=` |

### 1.5 无阻碍约束（改造不得破坏的东西）

> 这些是本项目已经用 CI 或安全设计固化的契约，改造面板时必须保持。

1. **CSP 必须保留**：CI（`ci.yml` L187–192）会 `grep 'Content-Security-Policy'
   apps/panel/index.html`，缺失即失败。`script-src 'self'` 不得放宽，
   **不得引入内联 `<script>` 或内联事件处理器属性**（`onclick="..."`）。
2. **`main.js` 必须通过 `node --check`**（`ci.yml` L166），且 `scripts/check-panel-auth.js`
   会校验：`queryToken` 用 `let`、登出清空三个 token 源、401 清空 token、
   `urlParams.delete("local_token")` + `history.replaceState`、Electron `restartAttempts` 归零。
   **重构渲染代码时这些字符串/标识符必须原样保留。**
3. **`webSecurity` 必须保持开启**（`ci.yml` L194–199）。
4. **`PanelAssets::get(req_path)` 的语义**：找不到路径时回退 `index.html` 并返回 **200**。
   新增资源若拼错路径，会**静默返回 HTML**而非 404——字体/图标资源加错位置时表现为
   "解码失败"而非"404"，排查成本高。
5. **Electron `file://` 加载路径**：无边框 + 自定义标题栏的 `-webkit-app-region: drag`
   必须继续覆盖顶部栏，否则窗口无法拖动。
6. **`apps/panel/**/*` 是打包 glob**：新增子目录（如 `apps/panel/fonts/`）会被
   RustEmbed 与 electron-builder 同时收录，但**目录必须真实存在于仓库**。

---

## 2. 目标规范基线

### 2.1 令牌层目标（Expressive）

**颜色**：保持 4 组色调（primary/secondary/tertiary/error）+ 中性色，补齐
`surface-dim/bright/tint`、`inverse-*`、`shadow`、`*-fixed` 系列，并为浅/深两套
主题各自提供**完整配对**。同时新增一个 `[data-theme=…][data-contrast="high"]`
覆盖层（M3 的 standard/medium/high 三档对比度）。

**字体**：Roboto Flex（可变字体），基线 15 档 + **emphasized 15 档**。
关键约束：**emphasized 只改字重与（必要的）字距，不改字号与行高**——所以它是
"drop-in"，切换时不会引起重排。字重映射：

| 基线字重 | Emphasized 字重 |
|---|---|
| Display/Headline/Body = 400 | → 500 |
| Title/Label = 500 | → 700 |

**形状**：`none 0` / `xs 4` / `s 8` / `m 12` / `l 16` / **`l+ 20`** / `xl 28` /
**`xl+ 32`** / **`xxl 48`** / `full`。

**运动**：
- **弹簧（交互反馈，主）**——三组，取自 Compose `ExpressiveMotionTokens`：

  | 令牌 | spatial damping | spatial stiffness | effects damping | effects stiffness |
  |---|---|---|---|---|
  | `spring-fast` | 0.6 | 800 | 1.0 | 3800 |
  | `spring-default` | 0.8 | 380 | 1.0 | 1600 |
  | `spring-slow` | 0.8 | 200 | 1.0 | 800 |

  （spatial = 位移/尺寸/形状；effects = 透明度/颜色/模糊。effects 组阻尼恒为 1.0，
  即**不产生视觉过冲**，这是规范要求。）

- **缓动（转场，辅）**：

  | 令牌 | 时长 | 曲线 |
  |---|---|---|
  | `emphasized` | 500ms | `cubic-bezier(0.2, 0, 0, 1)` |
  | `emphasized-decelerate` | 400ms | `cubic-bezier(0.05, 0.7, 0.1, 1)` |
  | `emphasized-accelerate` | 200ms | `cubic-bezier(0.3, 0, 0.8, 0.15)` |
  | `standard` | 300ms | `cubic-bezier(0.2, 0, 0, 1)` |
  | `standard-decelerate` | 250ms | `cubic-bezier(0, 0, 0, 1)` |
  | `standard-accelerate` | 200ms | `cubic-bezier(0.3, 0, 1, 1)` |

**间距**：4dp 网格 + 8dp 系统（`space4/8/12/16/20/24/28/32/40/48`）。

**高度**：level 0–5，以 **tonal surface** 表达（在 `surface` 上按 percentage 叠加
`primary`），阴影仅作补充。

### 2.2 六个维度的验收标准

| 维度 | 验收标准（可机械检查） |
|---|---|
| 外观语言 | ① `styles.css` 中语义层**零**硬编码色值（除 `:root` 令牌定义）；② 所有字重 ∈ {400,500,700} 且 700 只出现在 emphasized 类；③ 圆角全部走 `var(--md-sys-shape-corner-*)` |
| 页面结构 | ① 顶部栏 64px 且具备 scrolled 状态；② 导航在 `≥840px` 为 rail、`≥1200px` 为 drawer、`<600px` 为 bottom nav；③ 每个视图有 页头/内容/空态/错误态 四段结构 |
| 组件 | ① 按钮五档尺寸可寻址；② 图标按钮按压有形态变形；③ 对话框用 `<dialog>` 语义 + 焦点陷阱；④ 进度条支持 wavy 形态；⑤ 文本域为 M3 Filled/Outlined 浮动标签结构 |
| 交互 | ① 所有可点元素有 hover/focus-visible/pressed/disabled 四态；② 键盘可达（Tab 序合理、对话框 ESC 可关、导航方向键可达）；③ 全部图标按钮有可访问名称（非裸 `title`）；④ `aria-live` 覆盖通知 |
| 动效 | ① 交互反馈用 spring 令牌；② 视图切换用 emphasized 转场 + stagger；③ 尊重 `prefers-reduced-motion`；④ 只动 `transform`/`opacity`/`clip-path`/`border-radius` |
| 适配 | ① 断点按 window size class（600/840/1200/1600）；② 使用 `100dvh` 而非 `100vh`；③ Electron 窗口最小尺寸下调至 compact 可用；④ 深浅主题跟随系统且可在应用内覆盖 |

---

## 3. 维度一：外观语言

### 3.1 颜色系统

**3.1.1 补齐缺失角色**
在 `styles.css` 的 `:root`（L3–L40）与 `[data-theme="dark"]`（L66–L103）各补：

```
--md-sys-color-surface-dim
--md-sys-color-surface-bright
--md-sys-color-surface-tint
--md-sys-color-inverse-surface / -on-surface / -primary
--md-sys-color-shadow
--md-sys-color-primary-fixed / -dim / on-primary-fixed / on-primary-fixed-variant
--md-sys-color-secondary-fixed / -dim / on-secondary-fixed / on-secondary-fixed-variant
--md-sys-color-tertiary-fixed  / -dim / on-tertiary-fixed  / on-tertiary-fixed-variant
```

**3.1.2 消灭语义层硬编码**
按下表替换（这是第 1.3 节清单的逐条落地）：

| 现行 | 替换为 | 位置 |
|---|---|---|
| `rgba(255,255,255,.16/.3/.45)` | `var(--md-sys-color-outline-variant)` / `outline` / `on-surface-variant` | L145/150/154 |
| `rgba(255,255,255,.08/.14)` | `color-mix(in srgb, var(--md-sys-color-on-surface) 8%, transparent)` 及 14% | L254/258 |
| `#e81123` / `#bf0f1d` | `var(--md-sys-color-error)` / `--md-sys-color-on-error` | L262–268 |
| `linear-gradient(135deg, primary, tertiary)` | 纯 `--md-sys-color-primary-container` + `on-primary-container`（去渐变） | L281 |
| `rgba(130,213,229,.08/.12/.16/.32)` | `color-mix(… primary 8%/12%/16%/32%, transparent)` | L349/475/486/517/588/590/821 |
| `rgba(177,203,207,.28)` | `color-mix(… on-secondary-container 12%, transparent)` | L464 |
| `#ffc777` + `rgba(255,199,119,…)` | 新增 `--md-sys-color-warning / -container / on-warning-container` 令牌（M3 无 warning 角色，需自定义但**必须在令牌层定义**） | L594–596 |
| `#1a434d/#82d5e5` 等 8 个 plan 色 | 4 组 `--md-sys-color-plan-{plus,pro,team,free}-container/on` 令牌，浅深各一套 | L614–638 |
| `#e5a93b` | `var(--md-sys-color-warning)` | L669 |
| `rgba(0,0,0,.65)` | `color-mix(in srgb, var(--md-sys-color-scrim) 65%, transparent)` | L928 |
| `main.js` L657/658 行内 rgba | `.m3-chip--image` / `.m3-chip--tools` 类（§9.3） | `main.js` |
| `electron/main.js` L211 `"#0f1416"` | 读取新令牌值；并接 `nativeTheme` 动态设置（§9.4） | `electron/main.js` |

**3.1.3 对比度分档**
新增 `[data-contrast="high"]` 覆盖块，配合设置页新增的对比度选择（§9.2 设置页）。
至少保证：正文 4.5:1、大字号与图标 3:1、`outline-variant` 分隔线不承担信息传达。

### 3.2 字体排版

**3.2.1 字体族**
现状（L119）用系统字体栈，且 `<head>` **只加载了 Material Symbols，没有加载 Roboto**。
Expressive 的 emphasized 体系依赖可变字重，系统栈无法保证。

- 新增 `apps/panel/fonts/roboto-flex.woff2`（可变，`wght 100..1000`，可变 `opsz`）。
- `index.html` 增加 `@font-face`（或 `styles.css` 顶部）：
  ```css
  @font-face {
    font-family: 'Roboto Flex';
    src: url('fonts/roboto-flex.woff2') format('woff2-variations');
    font-weight: 100 1000;
    font-display: swap;
  }
  ```
- **自托管而非 Google Fonts CDN**：① Electron 以 `file://` 加载，离线时 CDN 会失败；
  ② 保留 CDN 就得放弃 `font-src 'self'` 的收紧机会。
- ⚠️ **风险点**：`file://` 协议下 CSP 的 `font-src 'self'` 是否匹配需实测；若不匹配，
  需要把该指令写成 `font-src 'self' file:`（`index.html` L15 与 `lib.rs` L476–492 两处
  必须同步，否则 HTTP 面板与 Electron 面板行为不一致）。
- 保留系统栈为 fallback：`'Roboto Flex', Roboto, -apple-system, …`。

**3.2.2 重建字阶（L161–L172）**
现字阶的**字号/行高/字距全都不符合 M3**，逐条修正：

| 类 | 现状 | 目标（基线） | 新增 emphasized 类 |
|---|---|---|---|
| `.m3-display-small` | 36/44, w700, -0.02em | 36/44, **w400, tracking 0** | `.m3-display-small-emphasized` 36/44 w500 |
| `.m3-headline-medium` | 28/36, w600, -0.01em | 28/36, **w400, 0** | `-emphasized` 28/36 w500 |
| `.m3-headline-small` | 24/32, w600 | 24/32, **w400** | `-emphasized` 24/32 w500 |
| `.m3-title-large` | **20/26**, w600 | **22/28, w500** | `-emphasized` 22/28 w700 |
| `.m3-title-medium` | 16/24, w600 | 16/24, **w500, tracking .15px** | `-emphasized` 16/24 w700 |
| `.m3-title-small` | **缺失** | 新增 14/20 w500 t.1 | 新增 emphasized 14/20 w700 |
| `.m3-body-large` | 16/24 w400 | 16/24 w400 **t.5px** | 新增 emphasized 16/24 w500 |
| `.m3-body-medium` | 14/20 w400 | 14/20 w400 **t.25px** | 新增 emphasized 14/20 w500 |
| `.m3-body-small` | **13/18** w400 | **12/16 w400 t.4px** | 新增 emphasized 12/16 w500 |
| `.m3-label-large` | 14/20 w600 t.01em | 14/20 **w500 t.1px** | 新增 emphasized 14/20 w700 |
| `.m3-label-medium` | **13/18** w600 t.02em | **12/16 w500 t.5px** | 新增 emphasized 12/16 w700 |
| `.m3-label-small` | **12/16** w600 t.04em | **11/16 w500 t.5px** | 新增 emphasized 11/16 w700 |
| **缺失** | — | `.m3-display-medium/large`、`.m3-headline-large` | 各自 emphasized |

> ⚠️ `body-small` 从 13→12、`label-medium` 从 13→12、`label-small` 从 12→11 是**字号
> 缩小**。`main.js` L304/L342 有 `font-size: 11px` 的内联覆盖，L446/448 有 `font-size: 14px`，
> 这些行内字号必须一并清理，否则会出现"类改了但覆盖还在"。

**3.2.3 数字排版**
指标卡（`.m3-metric-val` L797）、额度百分比（`main.js` L298/336）、模型上下文
（`main.js` L648）等数字应加 `font-variant-numeric: tabular-nums`，避免数值刷新时宽度跳动。

### 3.3 图标

- 现引入（`index.html` L19）已声明 `opsz,wght,FILL,GRAD` 轴，但 `styles.css`
  的 `.material-symbols-outlined`（L174–190）**只设了 `font-size`，从未设置
  `font-variation-settings`**。→ 补：
  ```css
  font-variation-settings: 'FILL' 0, 'wght' 400, 'GRAD' 0, 'opsz' 24;
  ```
- 新增 `.m3-icon--filled`（`'FILL' 1`）用于选中态，替换 L362–364 只对 nav-item 生效的
  局部写法。
- 图标尺寸纳入令牌：`--md-sys-icon-size: 24px`，并允许 20（密集）/24（标准）/40（强调）。
- 现状图标名混用语义化与非语义化（`neurology` 用于 providers、`travel_explore` 用于
  扫描），建议在 §9.2 的改动中统一到一致隐喻。

### 3.4 表面与高度

- 把 L42–46 / L105–108 的 4 级纯阴影改为 **6 级色调高度**：
  ```css
  --md-sys-elevation-level0: none;
  --md-sys-elevation-level1: 0 1px 2px 0 color-mix(in srgb, var(--md-sys-color-shadow) 30%, transparent), 0 1px 3px 1px color-mix(… 15% …);
  …
  ```
  并新增**色调叠加**令牌（在 `surface-container` 基础上叠加 primary 的 5%/8%/11%/12%/14%），
  由 `background-color` 表达高度，阴影降为辅助。
- 深色主题的阴影 alpha（0.35–0.65）需下调，因为深色下高度主要靠色调。
- `.m3-card-elevated`（L556）当前 `surface-container-low` + shadow-1，改为
  level1 色调表面；`.m3-card-filled`（L565）当前 `surface-container`，保持但补齐
  `.m3-card-elevated:hover` 的 level2/level3 过渡。

---

## 4. 维度二：页面结构

### 4.1 顶部应用栏（`index.html` L25–66，`styles.css` L202–269）

| 项 | 现状 | 目标 |
|---|---|---|
| 高度 | 48px（L206），同时容纳窗口按钮（L234–236 是 48×48） | **64px**（M3 small app bar），窗口按钮高度同步 64 |
| 状态 | 无 | 新增 `.scrolled` 状态：滚动后由 `surface` → `surface-container` 并加 level2 |
| 结构 | 品牌 + actions 混在一行 | 明确 `leading / headline / trailing` 三段式（M3 app bar 语义） |
| 标题 | `.m3-brand-title` 18px w700（L289–294） | 改用 `title-large`（22/28 w500） |
| 徽标 | `.m3-brand-badge`（L296–304） | 改为 M3 语义的 assist chip 或直接移除（"Desktop" 信息量低） |
| 图标 | `hub`（L28） | 保留，但纳入 §3.3 的 filled 规则 |

同时 `styles.css` L217–269 的窗口控制按钮要：① 高度跟随 64px；② hover 用令牌而非
`rgba(255,255,255,…)`；③ 关闭按钮用 `error` 而非 `#e81123`；④ 补 `:focus-visible`。

### 4.2 导航（`index.html` L70–94，`styles.css` L318–373）

当前是**固定 256px drawer，在 <900px 变成横向滚动条**（L1016–1039）。M3 Expressive
的方向是**按窗口尺寸换形**，而不是"把 drawer 压扁"：

| 窗口尺寸类 | 宽度 | 导航形态 |
|---|---|---|
| Compact | < 600 | **bottom navigation bar**（4 项正好，M3 上限 5） |
| Medium | 600–839 | **navigation rail**（80px，仅图标 + 选中 pill 指示器） |
| Expanded | 840–1199 | navigation rail（可选 expanded 96px 带标签） |
| Large | 1200–1599 | **navigation drawer 256px**（即现状） |
| Extra-large | ≥ 1600 | drawer + 内容区 max-width 约束（见 §4.4） |

改动点：
- `styles.css` L319–328 `.m3-navigation-drawer` 需要拆成
  `.m3-navigation-rail` / `.m3-navigation-drawer` / `.m3-navigation-bar` 三个类。
- `index.html` L70–94 的 `<nav>` 需要能被 JS 换形（同一份 DOM + 不同 class，
  或渲染两份由 CSS 显隐）。**推荐同一份 DOM + 容器查询/媒体查询换 class**，
  避免 `main.js` L252–267 的路由逻辑分叉。
- L330–346 `.m3-nav-item` 48px 高 → rail 中为 56px（icon+label）或 32px（仅 icon）
  + 选中态 pill（`secondary-container`，圆角 full，宽 56/高 32）。
- L362–364 `.active` 的 `FILL 1` 改为 §3.3 的统一规则。
- L366–373 `.m3-drawer-footer`（版本号）在 rail/bar 形态下应移到设置页或折叠。
- 路由切换（`main.js` L255–267）需加：① 方向键导航；② 选中项 `aria-selected`；
  ③ URL hash 同步（当前刷新页面会回到总览，属可用性缺陷）。

### 4.3 各页面结构

**总览页（`index.html` L100–202）**
- L113–157 指标网格：4 张卡 `repeat(auto-fit, minmax(250px,1fr))`（`styles.css` L764–769）。
  Expressive 下指标数字应用 `display-small`（36/44）+ emphasized，标签用 `label-medium`。
  卡片尺寸从"等高小卡"改为**主次分明的 bento**：Router 状态与当前账号为主卡（跨 2 列），
  Provider 计数与 Desktop 适配为次卡。
- L160–180 额度卡：`m3-card-filled` + 内嵌 3 个进度条。改为**主额度用大号 wavy 线性进度**，
  次级额度用紧凑行。
- L183–201 两张快捷卡：`grid-template-columns: 1fr 1fr` **硬编码内联**，无断点回落；
  改为 `.m3-quick-grid` 类 + 容器查询。

**账号页（L205–234）**
- L211–224 三个操作按钮（filled/tonal/outlined）挤在页头右侧。
  Expressive 建议：主操作保留 filled，其余收进 **split button** 或 **FAB menu**。
- L228 `.m3-accounts-grid`（`minmax(360px,1fr)`）在 600–840px 会掉到 1 列且卡片过宽，
  需按 size class 调整最小列宽。

**服务商页（L237–265）**
- 这是最适合引入 **Expressive 列表 + 展开详情** 的页面。当前每个 provider 一张大卡
  （`main.js` L844），内部再套 `.m3-model-row` 列表。
- 建议改为：provider 为 **list item（两级文字 + trailing 图标按钮）**，点击展开模型列表
  （共享轴转场，§5）。
- `main.js` L886–894 用 `style.display = "none"/"block"` 控制展开，需换成 class +
  高度过渡。

**设置页（L268–352）**
- L287–323 表单：两个开关行（L293/L304）用**内联 `border-top`** 做分隔
  （应为 `outline-variant` 的 divider 且走类）。
- L340–349 Desktop 操作按钮 → 适合 **button group**（connected，两段一体）。
- 新增控件：§3.1.3 的**对比度档位选择**、§7 的**动效开关**（减少动态）。

### 4.4 内容宽度与节奏

- L376–382 `.m3-main-content` 现为 `max-width: 1320px`，但**未 `margin: auto`**，
  在超宽窗口下内容贴左、右侧留白巨大。需改为居中约束。
- M3 建议大屏正文宽度约束 840–1040dp；卡片网格可放宽到 1320。建议双轨：
  `.m3-content-column`（max 1040，居中）与 `.m3-content-wide`（max 1440）。
- `padding: 28px 36px 64px`（L379）纳入 8dp 系统：24/32/64 或 32/40/64。
- L161–172 的字阶 + 本节宽度共同决定阅读体验；正文行宽应控制在 50–75 字符。

### 4.5 空状态 / 加载态 / 错误态

现状三种状态都是"一行文字"或"图标 + 两行文字"，且**加载态是静态文案**
（`index.html` L231/L262 写死"正在加载…"），没有骨架屏。

- **加载**：改为 **skeleton**（`main.js` `refreshAccounts` L458 / `refreshProviders` L953
  渲染前先铺骨架），替换 `index.html` L229–232 / L260–263 的静态占位。
- **空**：改为 M3 Expressive composed empty state：大图标（48–64px，`outline` 色）
  + `title-large` + `body-medium` + 一个填充按钮。位置：`index.html` L229–232、
  L260–263；`main.js` L507–513（账号）、L970–976（服务商）。
- **错误**：现状是 `main.js` L639/L985 直接插一段红字。改为带
  `error-container` 表面 + 重试按钮的 error state。
- **重试**：三处错误态都要有"重试"动作（调用对应 refresh 函数）。

---

## 5. 维度三：组件

> 本节逐个组件给出规格差异。命名统一为 `.m3-*`（沿用现有前缀）。

### 5.1 按钮

**尺寸（新增，`styles.css` L422–439）**

| 变体类 | 高度 | 水平内边距 | 图标 | 间距 | 字阶 |
|---|---|---|---|---|---|
| `.m3-btn-xs` | 32 | 12 | 16 | 4 | label-large |
| `.m3-btn-sm`（默认，即现状） | 40 | 16 | 18 | 8 | label-large |
| `.m3-btn-md` | 56 | 24 | 24 | 8 | title-medium |
| `.m3-btn-lg` | 96 | 48 | 32 | 12 | headline-small |
| `.m3-btn-xl` | 136 | 64 | 40 | 16 | headline-medium |

> ⚠️ 32/40 低于 48dp 触摸最小尺寸，**必须用 `::after` 扩展点击区而非撑高盒子**。
> 现状 L547/551/556/559/562 与 `main.js` L754/857/861 都用**内联 `height: 36px`**
> 绕过默认 40px——这些行内高度必须清掉，改为 `.m3-btn-xs`。

**形态与状态（`styles.css` L422–499）**
- `transition: all`（L435）→ 显式属性列表（`background-color, box-shadow, transform,
  border-radius, color`）。
- 现状按压只有 `transform: scale(0.98)`（L441–443）。Expressive 要求**形态变形**：
  按下时 `border-radius` 从 `full` 收到 `large-increased(20)`，配合 spring。
- 按钮**无 state layer**：hover 现状用 `filter: brightness(1.06)`（L454）与硬编码 rgba。
  改为标准 state layer：hover 8%、focus 12%、pressed 12% 的 `on-*` 颜色叠加。
- **无 `:focus-visible`**：全文件只有 `:focus`（L754），需补焦点环
  （2px `primary` outline + 2px offset）。
- **无 `:disabled` 通用样式**：只在 `main.js` L551 靠行内 `opacity: 0.8`。补
  `.m3-btn:disabled { opacity: .38; pointer-events: none }`（M3 disabled 用 38%）。
- **无 elevated 变体**：M3 有 Filled/Tonal/Elevated/Outlined/Text 五种，现缺 Elevated。

**新增 Expressive 组件**
- **Button group**（connected）：设置页 Desktop 操作（`index.html` L340–349）、
  账号卡片操作组（`main.js` L544–565）。
- **Split button**：账号页"保存当前登录态 + 下拉"（`index.html` L212–215）、
  服务商页"添加 Provider + 选择类型"（L244–251）。省掉一层菜单。
- **FAB menu**：服务商页/账号页的主操作为 FAB，展开 2–6 个动作。当前 `.m3-fab`
  （`styles.css` L522–546）**是死代码，0 处使用**；要么按 Expressive 重做并启用，
  要么删除。建议在服务商页启用。
- **Toggle button**：模型行的启用/停用（现为 switch，`main.js` L662）。

### 5.2 图标按钮（`styles.css` L502–519）

- 4 种变体：standard（现状）/ filled / filled-tonal / outlined。
- 尺寸：XS 32 / S 40（现状）/ M 48 / L 56 / XL 96。
- **按压形态变形**（M3 Expressive 明确为此特性）：
  `border-radius` 从 `full` → `small(8px)`，或依上下文用 squircle 形。
- 选中态（如主题切换、筛选）用 filled + `primary`。
- 触摸区：40px 的按钮需 `::after` 扩到 48dp。
- 使用位置：`index.html` L45/49，`main.js` L666/669/865/868。

### 5.3 卡片（`styles.css` L549–573）

- 圆角：现状统一 `extra-large(28)`（L550）。M3 卡片规范为 **medium(12)**，
  带媒体时用 large(16)。→ 改 `--md-sys-shape-corner-medium`，大容器（对话框、
  面板）保留 28。
- 高度：`elevated` 变体改用 §3.4 的色调高度。
- 卡片内边距：L551 固定 24，纳入 8dp 系统（16/24/32 按 density）。
- `.m3-account-card.active`（L818–822）用 `border-color: primary` + `box-shadow: 0 0 16px rgba(...)`
  表示"当前生效"。改为 M3 做法：**`primary-container` 色调表面 + 左侧 3px indicator**，
  去掉发光阴影。
- 新增 `.m3-card--interactive`（hover 抬升 + 按压沉降），用于可点击卡片。

### 5.4 徽标 / 标签 / Chip

- **区分两个概念**（现在混用 `.m3-badge`）：
  - **Badge**（M3 语义）：6dp dot 或 16dp 计数气泡，附着在图标/导航项上。→ 新增
    `.m3-badge-dot` / `.m3-badge-count`（如账号数量、待更新提示）。
  - **Chip**（状态/标签）：现状的 `.m3-badge-success/warning/error`（L587–602）
    实际是 **assist/status chip**。→ 重命名为 `.m3-chip` + `-success/-warning/-error`
    + `-assist`（带图标）+ `-input`（可删除，如模型能力标签 `main.js` L656–658）。
- Chip 高度 32dp、圆角 `small(8)`、`label-large`；现状是 `padding: 4px 12px` +
  `border-radius: full`（L579–584），需重做。
- `.m3-plan-badge`（L604–638）→ 用 §3.1.2 的令牌化版本 + `label-small` + `small` 圆角。

### 5.5 进度指示器（`styles.css` L641–674）

- 现状 8px 高直线（L653–659）。M3 线性进度为 **4px**；当前粗细是"强调"档。
- Expressive 提供 **wavy（波浪）** 形态，用于"活动/进行中"的额度展示。
  用 SVG `path` + `stroke-dasharray` 或 `clip-path` 实现；
  额度卡（`main.js` L272–346）改为主额度 wavy、次额度直线。
- **不定量进度**：现状用文字"正在加载…"。Expressive 要求用 **loading indicator**
  （形态在多个几何形之间变形，替代圆形 spinner），用于 <5s 的操作
  （`main.js` L776–787 扫描模型、L1190–1210 批量刷新）。
- 现状 `transition: width`（L665）→ 改 `transform: scaleX()` 以保证合成层动画。

### 5.6 开关（`styles.css` L677–727）

- 结构对（52×32 轨道 + 滑块），但：
  - 轨道 `border: 2px`（L698）+ 滑块 16→20px（L720–726）在过渡中会跳动；
    改为固定 24px 滑块 + `transform` 位移。
  - 未选中轨道用 `surface-container-highest` + `outline`，需按 M3 换成
    `surface-container-highest` + `outline` 的**正确配对**（L697–698 已是，但令牌值要复核）。
  - 补**选中/未选中的图标**（M3 switch with icon）。
  - 补 `:focus-visible`、`:disabled`。
  - 触摸区扩到 48dp。
- 使用处：`index.html` L298–301 / L309–312，`main.js` L662–665。

### 5.7 文本域（`styles.css` L730–757）

**这是改动量最大的组件。** 现状不是 M3 结构：`<label>` 在 `<input>` 上方，
input 自带 `surface-container-low` 背景 + 8px 圆角（L743–752）。

M3 两选一：
- **Filled**：容器 `surface-container-highest`，顶部圆角 `xs(4)`，底边 1px `on-surface-variant`；
  聚焦时底边 2px `primary`。标签浮动到容器内顶部。
- **Outlined**：透明容器 + 1px `outline` 边框 + `xs(4)` 圆角；聚焦时 2px `primary`。
  标签浮动并"切断"边框。

需要新增／改造：
1. `.m3-text-field` 结构改为：容器 + 浮动 `<label>` + `<input>` + supporting text。
   `index.html` 中 13 处 `.m3-text-field`（L288/370/393/397/401/408/428/432/436/440/468/472/493）
   与对话框内的表单全部要改结构。
2. **支持文本**（helper）+ **字符计数**：导入凭据对话框（L468–475，JSON textarea 8 行）
   必须有计数与错误提示——这是最容易输错的地方。
3. **错误态**：`error` 边框 + `error` 支持文本 + `aria-invalid`。
   现状错误只在对话框底部显示一行红字（L374/L476），与出错字段无关联。
4. **前后置图标 / 前缀**：密码框（L290/L372/L410）应有 visibility 切换。
5. **Select**（L403–406）：改用 M3 的 exposed dropdown menu 或至少统一视觉。
6. `textarea`（L474）的行内 `font-family: monospace; font-size: 12px` → 类。

### 5.8 对话框（`index.html` L363–516，`styles.css` L922–977）

**当前实现有实际可访问性缺陷，不只样式问题：**

| 缺陷 | 位置 | 后果 |
|---|---|---|
| 用 `<div>` 而非 `<dialog>`/`role="dialog"` | L364 等 6 处 | 屏幕阅读器不识别为对话框 |
| 无 `aria-modal`、无 `aria-labelledby` | 同上 | 标题不被朗读 |
| 无焦点陷阱 | `main.js` L105–169 | Tab 会跑到背景内容 |
| 无 ESC 关闭 | 同上 | 键盘用户无法取消 |
| 无背景滚动锁定 | `styles.css` L922–936 | 滚动穿透 |
| 无焦点归还 | `main.js` L120–124 `cleanup()` | 关闭后焦点丢失 |
| 显隐靠 `style.display` | `main.js` L117/121 等 | 无出入场动画、无过渡 |
| `display:none` 靠属性选择器兜底 | `styles.css` L938–940 | 脆弱 |

**目标**：改用原生 `<dialog>` + `showModal()`（Chromium/Electron 全支持），
获得免费的焦点陷阱、ESC、`::backdrop`、inert 背景；再用 `allow-discrete` +
`@starting-style` 做出入场动画。

- 圆角：`extra-large(28)` 保持，但配合 §2.1 的 `xl-increased(32)` 可选。
- Scrim：改令牌（L928）。
- 出场动画缺失（只有入场 L952/L960），需补 exit 转场。
- 尺寸：`max-width` 现状靠内联（L387/422/462 = 560/560/580）。→ 类
  `.m3-dialog--md` / `--lg`。
- 危险操作（删除账号/Provider）应使用 **`error` 强调的确认对话框**，
  并区分 primary action 颜色。

### 5.9 Snackbar（`styles.css` L982–1013）

- 背景应为 **`inverse-surface`**，文字 `inverse-on-surface`，动作按钮 `inverse-primary`
  （M3 规范）。现状用 `surface-container-highest`（L987）。
- 缺失：**action 按钮**（"撤销"/"重试"）、**队列**（连续 `notify` 会互相覆盖，
  见 `main.js` L93–102）、**时长分档**（4s 固定）。
- 位置：按窗口尺寸，compact 下应贴底全宽，expanded 下居中窄条。
- 需 `role="status"` + `aria-live="polite"`（现只有 `role="status"`，L519）
  与 `aria-atomic`。

### 5.10 新增组件（面板里目前完全没有）

| 组件 | 用在哪 | 为什么需要 |
|---|---|---|
| **Tooltip** | 14 处 `title=` 属性（`index.html` 6 处、`main.js` 8 处，如 L868/669） | 原生 `title` 延迟 1s+、不可键盘聚焦、不可主题化 |
| **Divider** | `main.js` L762 行内 `border-top`、`index.html` L293/304 | 统一 `outline-variant`，支持 inset |
| **List item** | 服务商页（`main.js` L646–673）、发现模型列表（L789–807） | 现在是裸 `div` + 行内 flex，无语义、无 hover/选中态 |
| **Menu** | 计划新增的 split button / FAB menu 下拉 | — |
| **Segmented button** | 主题（浅/深/跟随系统）、对比度（标准/中/高）、动效（全量/减弱） | 这些选择项现在没有 UI，且 segmented 是 Expressive 推荐控件 |
| **Skeleton** | §4.5 | 替换静态"加载中…" |
| **Loading indicator** | §5.5 | 替换不定量圆形/文字 |

---

## 6. 维度四：交互

### 6.1 状态覆盖

现状**没有一套统一的状态层**。逐项补齐（这是一张检查表，改造时逐类打勾）：

| 组件 | hover | focus-visible | pressed | disabled | selected |
|---|---|---|---|---|---|
| `.m3-btn` 系列 | ⚠️ 部分（filter/rgba） | ❌ | ⚠️ scale 0.98 | ❌ 仅行内 opacity | — |
| `.m3-icon-btn` | ✅ rgba | ❌ | ❌ | ❌ | ❌ |
| `.m3-nav-item` | ✅ rgba | ❌ | ❌ | — | ✅ |
| `.m3-card` | ⚠️ 仅 elevated | ❌ | ❌ | — | ❌ |
| `.m3-switch` | ❌ | ❌ | ❌ | ❌ | ✅ |
| `.m3-input` | ❌ | ❌（仅 `:focus`） | — | ❌ | — |
| `.m3-snackbar` | — | — | — | — | — |
| `.m3-snackbar` 动作 | ❌ | ❌ | ❌ | — | — |
| 模型行 / 账号卡 | ❌ | ❌ | ❌ | — | ❌ |

统一规格：
- **hover**：state layer 8%（`color-mix`）
- **focus**：state layer 12% + **2px `primary` 焦点环，offset 2px**
- **pressed**：state layer 12% + spring 形态/缩放反馈
- **disabled**：内容 38%、容器 12%
- **selected**：`secondary-container` / `primary-container` 表面 + `FILL 1` 图标

### 6.2 加载与异步反馈

- 现状**全局没有 loading 状态**：点击"刷新全量数据"（`main.js` L1088）只弹一个
  snackbar，按钮本身不进入 loading。
- 需要：按钮级 loading（图标替换为 loading indicator + 禁用 + `aria-busy`）。
  触及：`#topbar-sync-btn`（L1078）、`#overview-refresh-btn`（L1088）、
  `#refresh-all-accounts-usage-btn`（L1190，已有 `btn.disabled`，需加视觉）、
  `#providers-refresh-btn`（L1286）、Desktop 安装/恢复（L1320/L1330）。
- **乐观更新**：模型开关（L677–689）已在失败时回滚，但无过渡反馈。
- **骨架屏**：见 §4.5。

### 6.3 表单与校验

- 现状校验全靠 HTML5 `required` + 后端错误（`main.js` L1244/1281）。
- 需要：① 失焦即校验（blur）；② 错误与字段关联（`aria-describedby`）；
  ③ 提交前禁用重复提交；④ 密码强度/一致性提示（设置页 L288–291 的"留空则保持现状"
  逻辑要更明确）。
- 危险操作二次确认已存在（`m3Confirm`），但**删除 Provider 会连续弹两次确认**
  （`main.js` L933 与 L935）。建议合并为一个带 checkbox 的对话框
  （"同时清除凭证"），减少打断。

### 6.4 键盘与可访问性

现状全项目**零** `aria-*`、**零** `role`（除 L519 一个 `role="status"`）、
**零** `tabindex`、**零** `<dialog>`。需要：

1. **Skip link**：`<a href="#main" class="m3-skip-link">跳到主内容</a>`（首个可聚焦元素）。
2. **语义化标签**：`nav` 已有（L70），`main` 已有（L97），但内容区全是 `div`；
   `section` 有（L100 等）但缺 `aria-labelledby` 指向页头 `h2`。
3. **图标按钮可访问名**：14 处 `title=` 只是 tooltip 提示；需要 `aria-label`
   （`main.js` L556/559/562/666/669/857/861/865/868）。
4. **对话框**：§5.8。
5. **导航**：方向键 + Home/End，`aria-selected`。
6. **实时区域**：snackbar 用 `aria-live`；加载完成/失败也要播报。
7. **焦点可见**：全局 `:focus-visible` 规则。
8. **表格/列表语义**：模型列表（L877–884）用 `<ul>/<li>`。
9. **语言**：`<html lang="zh-CN">` 已有（L2）✅；纯英文标识符（Provider、Router）
   建议 `<span lang="en">`。
10. **触摸目标**：全部 ≥48dp（§5.1 已述）。
11. **对比度**：§3.1.3。

### 6.5 主题切换

- 现状：`main.js` L232–247，localStorage 键 `codex_mp_theme`，**只有 dark/light 二值**，
  默认 dark（L234 + `index.html` L2）。
- 目标：三态 **system / light / dark**，默认 `system`：
  ```js
  const mq = matchMedia("(prefers-color-scheme: dark)");
  ```
  `system` 时监听 `mq.change` 自动跟随。
- Electron 侧接 `nativeTheme`（`electron/main.js` 目前**完全没有** nativeTheme 引用），
  把系统主题通过 preload 暴露或直接由渲染层的 `prefers-color-scheme` 处理
  （Electron 会自动跟随 `nativeTheme.themeSource`）。
- 切换按钮（`index.html` L45–47）从"二值图标按钮"改为下拉菜单或设置页的分段按钮。

### 6.6 外部链接与危险操作

- 面板内有多处展示 URL（`main.js` L405/445/449、L854）。建议对可访问 URL
  提供"复制"/"打开"动作，并走 Electron 的 `openExternalSafely`。
- 现状**没有**外部链接元素（全为纯文本），所以这条是增强而非修复。

---

## 7. 维度五：动效

### 7.1 令牌重建（`styles.css` L57–63）

删除现有 3 duration + 2 easing，替换为：

```css
/* 弹簧：交互反馈（spatial 用于位移/尺寸/形状，effects 用于颜色/透明度） */
--md-sys-motion-spring-fast-spatial-damping: 0.6;
--md-sys-motion-spring-fast-spatial-stiffness: 800;
--md-sys-motion-spring-default-spatial-damping: 0.8;
--md-sys-motion-spring-default-spatial-stiffness: 380;
--md-sys-motion-spring-slow-spatial-damping: 0.8;
--md-sys-motion-spring-slow-spatial-stiffness: 200;
/* effects 组 damping 恒为 1.0（不过冲） */
--md-sys-motion-spring-default-effects-stiffness: 1600;
--md-sys-motion-spring-fast-effects-stiffness: 3800;
--md-sys-motion-spring-slow-effects-stiffness: 800;

/* 转场：easing + duration */
--md-sys-motion-duration-short1: 50ms;
--md-sys-motion-duration-short2: 100ms;
--md-sys-motion-duration-short3: 150ms;
--md-sys-motion-duration-short4: 200ms;
--md-sys-motion-duration-medium1: 250ms;
--md-sys-motion-duration-medium2: 300ms;
--md-sys-motion-duration-medium3: 350ms;
--md-sys-motion-duration-medium4: 400ms;
--md-sys-motion-duration-long1: 450ms;
--md-sys-motion-duration-long2: 500ms;
--md-sys-motion-duration-long3: 550ms;
--md-sys-motion-duration-long4: 600ms;
--md-sys-motion-easing-emphasized: cubic-bezier(0.2, 0, 0, 1);
--md-sys-motion-easing-emphasized-decelerate: cubic-bezier(0.05, 0.7, 0.1, 1);
--md-sys-motion-easing-emphasized-accelerate: cubic-bezier(0.3, 0, 0.8, 0.15);
--md-sys-motion-easing-standard: cubic-bezier(0.2, 0, 0, 1);
--md-sys-motion-easing-standard-decelerate: cubic-bezier(0, 0, 0, 1);
--md-sys-motion-easing-standard-accelerate: cubic-bezier(0.3, 0, 1, 1);
```

**弹簧在 CSS 中的落地方式**（因为 CSS 没有 spring 函数）：
1. **首选**：CSS `linear()` 缓动函数，用阻尼谐振子采样点逼近（Chromium 113+ 支持，
   Electron 33 内置 Chromium 130，面板与浏览器端均可用）。为三组弹簧各生成一条
   `linear(...)` 曲线，写成 `--md-sys-motion-spring-*-spatial-easing`。
2. **备选**：`@keyframes` 手工采样（兼容性最好，但每条曲线要单独写）。
3. **过冲表现**：spring 的 spatial 组会产生 >1 的位移，`transform` 天然支持；
   注意 `scale()` 过冲不会导致布局溢出，但**不要**对 `width/height` 用过冲值。

### 7.2 应用点（逐个替换现状）

| 位置 | 现状 | 目标 |
|---|---|---|
| `styles.css` L441–443 按钮按压 | `transform: scale(0.98)` | spring-fast spatial + 圆角变形（full → 20） |
| L502–519 图标按钮 | 仅背景色过渡 | spring-default spatial 形态变形（full → 8） |
| L543–546 FAB hover | `translateY(-2px)` | spring-slow spatial 抬升 |
| L553 卡片 hover | 仅 shadow | spring 色调高度过渡 |
| L665 进度条 | `transition: width` | `transform: scaleX()` + spring-slow |
| L700/712 开关 | `transition: all` | 滑块 `transform` 用 spring-fast；轨道颜色用 effects |
| L387–397 视图切换 | 单一 `translateY(6px)` + fade | emphasized-decelerate 入场 + **stagger**（卡片 30–50ms 递增） |
| L935 scrim 入场 | 固定 150ms | `standard` 300ms / `emphasized` 500ms 分档 |
| L952/960 对话框入场 | `scale(0.92)` | spring-default spatial（带轻微过冲）+ 圆角变形 |
| L1000–1002 snackbar | `translateY(100px)` | emphasized-decelerate 入场 / accelerate 出场 |
| 视图切换（`main.js` L263–265） | 瞬时 `display` 切换 | 共享轴转场（X 轴平移 + 淡入淡出） |
| Provider 展开（`main.js` L886–894） | `display` 瞬间切换 | 高度 + 淡入，spring-slow |
| 模型列表渲染（`main.js` L880） | 一次性 append | 逐项 stagger 入场（30ms） |
| 导航选中指示器（L353–356） | 背景色瞬切 | 指示器 pill 用 spring 在项目间滑动 |

### 7.3 动效可访问性

- **必须**新增：
  ```css
  @media (prefers-reduced-motion: reduce) {
    *, *::before, *::after {
      animation-duration: 0.01ms !important;
      animation-iteration-count: 1 !important;
      transition-duration: 0.01ms !important;
      scroll-behavior: auto !important;
    }
  }
  ```
- 应用内还需一个**动效开关**（设置页），把 `prefers-reduced-motion` 与用户偏好
  合并（现状 `main.js` 完全没有相关处理）。
- 现状 `@keyframes` 只有 3 个（`m3FadeIn` L394、`m3ScrimFadeIn` L955、
  `m3DialogScaleIn` L960），新增动画需集中管理并命名前缀 `m3e-`（expressive）。

### 7.4 性能约束

- 只动 `transform` / `opacity` / `border-radius` / `clip-path`。
- 现状 `transition: all`（L435、L700、L712、L815）与 `transition: width`（L665）
  必须改掉，否则会触发布局。
- stagger 总时长控制在 200ms 内，避免长列表卡顿。
- 长列表（模型可能上百项）不要逐项动画，超过 20 项时降级为容器级淡入。

---

## 8. 维度六：适配

### 8.1 窗口尺寸类与断点

删除现有的单一 `@media (max-width: 900px)`（L1016–1039），替换为：

| 尺寸类 | 宽度 | 导航 | 内容列 | 网格 |
|---|---|---|---|---|
| Compact | < 600 | bottom nav bar | 1 列，边距 16 | 指标卡 1 列 |
| Medium | 600–839 | navigation rail (80) | 1 列，边距 24 | 指标卡 2 列 |
| Expanded | 840–1199 | rail (96，带标签) | max 840 居中 | 指标卡 2–4 列 |
| Large | 1200–1599 | drawer (256) | max 1040 居中 | 指标卡 4 列 |
| Extra-large | ≥ 1600 | drawer (256) + 可选副栏 | max 1040 居中 | 指标卡 4 列 + 更大留白 |

**用容器查询优先**：卡片网格（`styles.css` L764–769 `.m3-metric-grid`、
L804–808 `.m3-accounts-grid`）应基于**容器**而非视口，这样在 rail 展开/收起时
不会错位。Chromium 130 支持 `@container`。

> 注意：本项目是 **Electron 桌面应用 + 浏览器远程访问** 双形态。浏览器远程访问时
> 窗口可能被压到 360px（手机），所以 compact 档必须有真实可用的布局，
> 不能只是"缩小"。

### 8.2 视口单位与滚动

- `body { min-height: 100vh }`（L122）→ **`100dvh`**。
- `.m3-layout-root { min-height: 100vh }`（L198）→ 同上。
- 内容区滚动：现状整页滚动（`body` 滚动）。桌面应用更合适的是
  **app bar 固定 + 内容区独立滚动**（这样 app bar 的 scrolled 状态才有意义）。
- 滚动条：现状自定义滚动条（L134–159）硬编码白色。→ 令牌化，并在浅色下正确显示；
  或改用 `scrollbar-width: thin` + `scrollbar-color`（Electron 130 支持）。

### 8.3 Electron 窗口（`apps/electron/main.js` L200–213）

| 项 | 现状 | 目标 |
|---|---|---|
| 初始尺寸 | 1220×840 | 保持（落在 Large 档） |
| 最小尺寸 | 920×660（L204–205） | **下调到 400×600**，否则 compact/medium 两档永远无法触达，整套响应式形同虚设 |
| 背景色 | `"#0f1416"` 硬编码（L211） | 跟随主题；或改为 `nativeTheme.shouldUseDarkColors ? darkSurface : lightSurface` |
| 无边框 | `frame: false`（L209） | 保持（Expressive 不要求改） |
| 拖拽区 | `.window-drag-region`（`styles.css` L218–220） | 保持；但高度从 48→64 后需确认拖拽区仍覆盖整条 |
| 菜单栏 | `Menu.setApplicationMenu(null)`（L222） | 保持 |

同时需处理：窗口尺寸变化时导航换形（§4.2）要即时生效，不能只在启动时判定。

### 8.4 缩放与 DPI

- Electron 默认支持 `Ctrl +/-` 缩放；`rem` 化后布局能跟随。建议把 `styles.css`
  里的 px 字号改为 `rem`（基准 16px），保证用户缩放时字阶等比。
- 高 DPI 下的 1px 分隔线：用 `0.5px` 或 `box-shadow: inset` 避免在 125%/150% 缩放下发虚。

### 8.5 主题与系统偏好

- `prefers-color-scheme`（§6.5）
- `prefers-reduced-motion`（§7.3）
- `prefers-contrast: more` → 映射到 §3.1.3 的高对比令牌
- `forced-colors`：Windows 高对比模式下需保证边框/焦点可见

### 8.6 国际化与文本

- `lang="zh-CN"`（`index.html` L2）✅
- 界面文案里英文混排（"Provider"、"Router"、"Desktop"、"Tools"）建议加 `<span lang="en">`
  以正确选择字形。
- **文案长度弹性**：`.m3-btn` 现状 `padding: 0 20px` 无 `max-width`，某些按钮文字
  很长（`index.html` L214 "保存当前登录态为账号"、L221 "批量刷新所有额度"）。
  在 compact 档需要换行或缩短文案。
- 数字与时间格式：`formatRemainingTime`（`main.js` L221–229）用中文单位，
  与本项目单语言定位一致 ✅。

### 8.7 打印与导出

- 当前无打印样式。管理面板可能需要打印/导出账号与额度信息。建议加 `@media print`
  隐藏导航与操作按钮。属可选增强。

---

## 9. 全量改动清单（按文件）

> 这是"全部需要修改的地方"的汇总表。**每条都可独立勾选。**

### 9.1 `apps/panel/styles.css`（1039 行，改动最密集）

| # | 行号 | 现状 | 动作 |
|---|---|---|---|
| 1 | L3–40 | 浅色令牌，缺 Expressive 角色 | 补齐 §3.1.1 全部角色 |
| 2 | L42–46 | 4 级纯阴影 | 改 6 级色调高度 + 辅助阴影（§3.4） |
| 3 | L49–55 | 形状令牌缺 20/32/48 | 补 `large-increased` / `extra-large-increased` / `extra-extra-large` |
| 4 | L57–63 | 运动令牌错误（emphasized == standard） | 整体替换为 §7.1 |
| 5 | L66–109 | 深色令牌，缺角色 + 阴影过重 | 同 #1/#2 |
| 6 | L112–116 | `* { margin:0; padding:0 }` 全局重置 | 保留，但注意会清掉 `<dialog>` 默认样式，需显式补 |
| 7 | L118–127 | `font-family` 系统栈；`100vh` | 改 Roboto Flex + fallback；`100vh`→`100dvh` |
| 8 | L134–159 | 硬编码白色滚动条 | 令牌化 + 浅色主题修正 |
| 9 | L161–172 | 字阶字号/字重/字距全错，缺 emphasized | 按 §3.2.2 表逐条重建（15 基线 + 15 emphasized） |
| 10 | L174–190 | Material Symbols 无 `font-variation-settings` | 补轴设置 + `.m3-icon--filled` |
| 11 | L195–199 | `.m3-layout-root` `100vh` | `100dvh` |
| 12 | L202–215 | app bar 48px，无 scrolled 态 | 64px + 三段式 + `.scrolled` |
| 13 | L217–269 | 窗口按钮硬编码 rgba + `#e81123` | 令牌化；关闭按钮用 `error`；高度 64；补 focus-visible |
| 14 | L271–304 | 品牌图标渐变 + 硬编码尺寸 | 去渐变；`title-large`；badge 改 chip |
| 15 | L306–310 | topbar actions | 纳入 8dp gap |
| 16 | L313–316 | app body flex | 保留，但配合导航换形 |
| 17 | L319–328 | 固定 256px drawer | 拆为 rail / drawer / bottom bar 三形态 |
| 18 | L330–346 | nav item 48px，硬编码 hover | 分形态尺寸；state layer；48dp 触摸区 |
| 19 | L348–364 | 选中态仅背景色 | 加指示器 pill + spring 滑动 + `FILL 1` |
| 20 | L366–373 | drawer footer | rail/bar 形态下隐藏；样式令牌化 |
| 21 | L376–382 | `max-width` 无 `margin:auto` | 居中约束 + 双轨宽度（§4.4） |
| 22 | L385–397 | 视图切换单 fade | 共享轴转场 + stagger（§7.2） |
| 23 | L399–415 | section header | 结合 page header 语义 |
| 24 | L422–439 | 按钮单档 40px、`transition: all` | 五档尺寸 + 显式过渡（§5.1） |
| 25 | L441–443 | `scale(0.98)` | spring + 形态变形 |
| 26 | L446–499 | 按钮变体：brightness/rgba/缺 elevated | 五变体 + state layer + focus-visible + disabled |
| 27 | L502–519 | icon button 单档 40px | 4 变体 × 5 尺寸 + 按压变形 |
| 28 | L522–546 | `.m3-fab` 死代码 | 重做为 Expressive FAB + FAB menu，或删除 |
| 29 | L549–573 | 卡片统一 28px 圆角 + 阴影高度 | 12/16px 圆角 + 色调高度 + `--interactive` |
| 30 | L576–602 | badge 概念混淆 + 硬编码 | 拆 Badge / Chip；令牌化 |
| 31 | L604–638 | plan badge 6 个硬编码色（plus/pro/team） | 令牌化（4 组 × 深浅） |
| 32 | L641–674 | 8px 直线进度 + `transition: width` | 4px 线性 + wavy 变体 + `scaleX` |
| 33 | L677–727 | switch 尺寸跳动 + 无图标/焦点 | 结构修正 + 图标 + focus-visible + 48dp |
| 34 | L730–757 | 文本域非 M3 结构 | 重建为 Filled/Outlined 浮动标签 + 支持文本 + 错误态 |
| 35 | L764–769 | 指标网格 `auto-fit` 无断点 | 容器查询分档 |
| 36 | L771–801 | 指标卡 | emphasized 排版 + tabular-nums + bento 主次 |
| 37 | L804–859 | 账号卡（含 active 发光） | indicator 方案 + 网格断点 |
| 38 | L862–909 | provider/model 列表 | 改 list item + divider |
| 39 | L912–917 | settings 容器 | 宽度约束 |
| 40 | L922–940 | backdrop + `[style*=...]` 兜底 | 改 `::backdrop`；删兜底选择器 |
| 41 | L942–953 | dialog 固定 max-width + 仅入场 | 尺寸类 + 出入场转场 |
| 42 | L955–963 | 2 个 keyframes | 集中为 `m3e-*` 前缀 |
| 43 | L965–977 | dialog head/actions | 令牌化 |
| 44 | L982–1013 | snackbar 用 surface 而非 inverse | `inverse-*` + action + 队列 |
| 45 | L1016–1039 | 单一 900px 断点 | 替换为五档 size class（§8.1） |
| 46 | 全文 | 缺 `:focus-visible` | 全局补 |
| 47 | 全文 | 缺 `prefers-reduced-motion` | 补（§7.3） |
| 48 | 全文 | 缺 `prefers-contrast` / `forced-colors` | 补（§8.5） |
| 49 | 全文 | px 字号 | 视情况 `rem` 化（§8.4） |
| 50 | 新文件 | — | 建议拆分为 `tokens.css` / `components.css` / `layout.css`，但**必须同步更新 `RustEmbed` 与 electron `files`（均已用目录级 glob，无需改配置）** |

### 9.2 `apps/panel/index.html`（526 行）

| # | 行号 | 现状 | 动作 |
|---|---|---|---|
| 1 | L2 | `data-theme="dark"` 硬编码 | 改为由脚本按 system 偏好设置；默认 `system` |
| 2 | L13–16 | CSP meta（含 Google Fonts 源） | 若自托管字体则收紧 `font-src 'self'`；**必须与 `lib.rs` L476–492 同步**；注意 `file://` 下 `'self'` 的实测 |
| 3 | L19 | 仅引入 Material Symbols | 保留；字体改为自托管引入（`fonts/`） |
| 4 | L23 | `.m3-layout-root` | 保持 |
| 5 | L25–66 | 顶栏整体 | 高度 64、三段式语义、scrolled 态、`hub` 图标 filled 化 |
| 6 | L26 | `window-no-drag` | 保持（拖拽契约） |
| 7 | L36 | 行内 `font-size:16px` | 移除，用类 |
| 8 | L40–43 | 同步按钮 | 加 loading 态 + `aria-busy` |
| 9 | L45–47 | 主题切换二值按钮 | 改三态（system/light/dark），用 menu 或 segmented |
| 10 | L49–51 | 行内 `display:none` | 改类 `.is-hidden` + `hidden` 属性 |
| 11 | L54–64 | 窗口控制按钮 | 保持结构；高度同步 64；补 `aria-label` |
| 12 | L70–94 | `<nav>` drawer | 支持三形态；`aria-label`；nav item 补 `aria-current` |
| 13 | L88–93 | footer 行内样式 | 类化；rail/bar 形态隐藏 |
| 14 | L97 | `<main>` | 加 `id="main"`（供 skip link） |
| 15 | L100–110 | 总览页头 | 加 `aria-labelledby`；刷新按钮加 loading |
| 16 | L113–157 | 指标网格 | bento 主次 + emphasized 字阶 |
| 17 | L116/127/138/149 | 行内 `style="color:…"` | 类化 |
| 18 | L132 | 行内 `font-size/line-height/word-break` | 类化 |
| 19 | L133/144/155 | 行内颜色 | 类化 |
| 20 | L160–180 | 额度卡 | wavy 主进度 + 紧凑次进度 |
| 21 | L183–201 | 快捷卡 2 列内联 grid | 类化 + 断点 |
| 22 | L205–234 | 账号页头 3 按钮 | 主操作 + split/overflow |
| 23 | L229–233 | 静态"正在加载" | 骨架屏 |
| 24 | L237–265 | 服务商页头 | 同上 |
| 25 | L260–264 | 静态"正在加载" | 骨架屏 |
| 26 | L268–352 | 设置页 | 新增对比度 / 动效 / 主题分段按钮 |
| 27 | L287–323 | 安全表单 | 文本域重建；开关行类化 |
| 28 | L293/304 | 行内 `border-top` 分隔 | `.m3-divider` |
| 29 | L315 | 行内颜色 | 类化 |
| 30 | L317 | 行内 `justify-content` | 类化 |
| 31 | L336 | 行内 `margin-bottom` + 颜色 | 类化 |
| 32 | L340–349 | Desktop 两按钮 | button group |
| 33 | L363–383 | 登录对话框 | `<dialog>` + 语义 + 校验 |
| 34 | L386–418 | 添加 Provider 对话框 | 同上；文本域重建 |
| 35 | L421–458 | 添加模型对话框 | 同上；checkbox 改 M3 checkbox（现为原生，L444–451） |
| 36 | L461–483 | 导入凭据对话框 | 同上；textarea 加计数/校验 |
| 37 | L486–502 | prompt 对话框 | 同上 |
| 38 | L505–516 | confirm 对话框 | 同上 + 危险操作 error 强调 |
| 39 | L519–522 | snackbar | `inverse-*` + `aria-live` + action 槽 |
| 40 | L524 | `<script src>` | 保持 |
| 41 | 新增 | — | skip link（作为 `<body>` 首元素） |
| 42 | 70 处 | 内联 `style="…"` | 全部迁移到类（含 6 处对话框宽度） |
| 43 | 6 处 | `title=` | 补 `aria-label`，后续升级为 tooltip |

### 9.3 `apps/panel/main.js`（1372 行）

| # | 行号 | 现状 | 动作 |
|---|---|---|---|
| 1 | L88–102 | snackbar 全局单例、无队列/动作 | 改队列 + action + `aria-live` |
| 2 | L105–138 | `m3Prompt` 手写显隐 | 改 `<dialog>.showModal()` + 焦点归还 |
| 3 | L141–169 | `m3Confirm` 同上 | 同上 + 危险操作变体 |
| 4 | L232–247 | 二值主题 | 三态 + `matchMedia` 监听 |
| 5 | L252–267 | 路由瞬时切换 | 共享轴转场 + hash 同步 + 键盘导航 |
| 6 | L275–308 | 进度条模板（行内 style ×6） | 类化 + wavy 变体 + `scaleX` |
| 7 | L310–346 | reserve 进度模板（行内 ×6） | 同上 |
| 8 | L356–386 | router 状态更新 | badge→chip 类名同步 |
| 9 | L389–417 | desktop 状态 | 同上 |
| 10 | L420–455 | security 状态 | 同上 |
| 11 | L432–436 | 行内 `display` 控制 | 类化 |
| 12 | L458–641 | 账号渲染（行内 style 约 20 处） | 全部类化；active 卡改 indicator |
| 13 | L507–513 | 空状态 | composed empty state |
| 14 | L544–565 | 账号操作按钮组（行内 height 36） | button group / icon button 类 |
| 15 | L639 | 错误态一行红字 | error state + 重试 |
| 16 | L644–739 | 模型行（行内 style ×10） | list item + chip + toggle button + tooltip |
| 17 | L651–658 | 能力 chip 硬编码 rgba | 类化 |
| 18 | L662–665 | switch 无 48dp | 类化 + 触摸区 |
| 19 | L742–839 | 发现面板（行内 ×10） | 类化 + 展开动画 |
| 20 | L776–787 | 扫描无 loading 指示 | loading indicator |
| 21 | L789–807 | 列表项裸 div + 原生 checkbox | list item + M3 checkbox |
| 22 | L842–950 | provider 卡（行内 ×8） | 重构为 list item + 展开 |
| 23 | L886–894 | `display` 切换展开 | class + 高度过渡 |
| 24 | L905–928 | **连续两次 prompt**（编辑 provider） | 合并为单个表单对话框 |
| 25 | L932–947 | **连续两次 confirm**（删除 provider） | 合并为带 checkbox 的单对话框 |
| 26 | L953–987 | provider 刷新 | 骨架屏 + 空/错误态 |
| 27 | L1008–1023 | dialog 显隐 | `<dialog>` API |
| 28 | L1078–1085 | 同步按钮 | loading 态 |
| 29 | L1088–1091 | 总览刷新 | loading 态 |
| 30 | L1190–1210 | 批量刷新（逐账号串行） | loading 指示 + 进度反馈 |
| 31 | L1213–1289 | 两个新增对话框表单 | 结构重建 + 校验 |
| 32 | L1292–1317 | 安全表单提交 | 字段级校验 + 错误关联 |
| 33 | L1320–1340 | Desktop 安装/恢复 | loading + button group |
| 34 | L1343–1368 | Electron 窗口控制 | 保持；`restartAttempts` 相关字符串不得动 |
| 35 | L1372 | 启动 `refreshAll()` | 保持 |
| 36 | 全文 | 74 处行内 `style="` | 清理 |
| 37 | 新增 | — | `prefers-reduced-motion` 与主题的读取 |
| 38 | 全文 | `queryToken`/`sessionToken`/登出/401 逻辑 | **严禁改动**（CI 校验） |

### 9.4 `apps/electron/main.js`（434 行）

| # | 行号 | 现状 | 动作 |
|---|---|---|---|
| 1 | L200–213 | 窗口配置 | 见 §8.3 |
| 2 | L204–205 | `minWidth: 920, minHeight: 660` | 下调到约 400×600 |
| 3 | L211 | `backgroundColor: "#0f1416"` | 跟随主题令牌 |
| 4 | 全文 | 无 `nativeTheme` 引用 | 引入 `nativeTheme`，暴露系统主题 |
| 5 | L222 | 移除菜单栏 | 保持 |
| 6 | `restartAttempts` 归零逻辑 | — | **严禁改动**（CI 校验） |
| 7 | `webSecurity` | — | **严禁关闭**（CI 校验） |

### 9.5 `apps/electron/preload.js`（21 行）

| # | 动作 |
|---|---|
| 1 | 若主题需由主进程决定，新增 `systemTheme` 字段（注意：新增 IPC 通道即新增攻击面，需在主进程做来源校验，与 `get-bootstrap` 同规格） |
| 2 | 若不新增通道，依赖渲染层 `prefers-color-scheme`，**本文件不改** |

### 9.6 `crates/web/src/lib.rs`（2191 行）

| # | 行号 | 动作 |
|---|---|---|
| 1 | L37–39 | `RustEmbed(folder="../../apps/panel")` — 新增字体/资源目录自动纳入，无需改；**但需确认 CI 构建时文件存在** |
| 2 | L476–492 | CSP 响应头 — 必须与 `index.html` 的 meta **逐字同步**（字体策略变更时两处同改） |
| 3 | L498–515 | 静态托管回退语义（找不到 → 返回 `index.html` 200）— 新增资源时留意；建议对已知静态后缀（`.woff2/.css/.js/.svg`）改为真 404 |
| 4 | L536–546 | 响应头集合 — 可补 `Cache-Control` / `ETag`（字体文件较大，且 RustEmbed 内容哈希可用） |
| 5 | L125–145 | `SecurityStatusResponse` — 若新增 UI 偏好（对比度/动效）且需服务端持久化，则扩展；否则保持前端 localStorage |

### 9.7 静态资源

| # | 文件 | 动作 |
|---|---|---|
| 1 | `apps/panel/fonts/roboto-flex.woff2` | **新增**（自托管可变字体） |
| 2 | `assets/icon.svg` | 调色板对齐新令牌；`rx=16` → `rx=22`/squircle |
| 3 | `assets/icon.png` | 由 SVG 重新导出（现为 3436 字节旧图） |
| 4 | `apps/panel/` | 可选新增 `icons.svg` sprite（若要从字体图标迁移到 SVG） |

### 9.8 构建与 CI

| # | 文件 | 动作 |
|---|---|---|
| 1 | `package.json` `build.files` | `apps/panel/**/*` 已覆盖新增子目录 → **无需改**；仅在新增顶层资源目录时才改 |
| 2 | `.github/workflows/ci.yml` L166 | `node --check apps/panel/main.js` 保持通过 |
| 3 | `.github/workflows/ci.yml` L187–192 | CSP 存在性检查保持通过 |
| 4 | `.github/workflows/ci.yml` L194–199 | `webSecurity` 检查保持通过 |
| 5 | `scripts/check-panel-auth.js` | 保持通过；**可选新增**：检查 `prefers-reduced-motion` 存在、检查语义层无硬编码色值 |
| 6 | 新增校验（建议） | `node scripts/check-panel-tokens.js`：断言 `styles.css` 中 L112 之后不再出现裸 `#hex` / `rgba(` |

### 9.9 文档

| # | 文件 | 动作 |
|---|---|---|
| 1 | `README.md` L18/L20 | 若面板外观描述变化，同步更新 |
| 2 | `docs/` | 新增本文件；实施后追加验证报告，格式对齐 `VERIFICATION_AND_FIX_PLAN.md`（证据分级 ✅/⚠️） |

---

## 10. 实施阶段与顺序

按"低风险高收益 → 高风险结构性"排序。每个阶段结束都应**可运行、可回滚**。

### 阶段 0：准备（0.5 天）
- [ ] 引入自托管字体，验证 HTTP 面板与 Electron `file://` 两条链路都能加载
- [ ] 实测 `file://` 下 CSP `font-src` 的匹配行为（决定了 §9.2 #2 的写法）
- [ ] 建立视觉回归基线：对 4 个页面 × 2 主题 × 3 档宽度截图存档

### 阶段 1：令牌层（1–2 天）——**零视觉回归风险**
- [ ] `styles.css` L3–109 整体重建：颜色全角色、形状补 3 档、运动双轨（spring + easing）、高度 6 级、间距系统
- [ ] 干掉 §1.3 全部硬编码（语义层零裸色值）
- [ ] 字阶重建（15 基线 + 15 emphasized）
- [ ] 此阶段结束**外观应与现在几乎一致**，但底层已就位

### 阶段 2：字阶与颜色应用（1 天）
- [ ] 把 w600/w700 的既有用法改为基线 400/500，需要强调处换 emphasized 类
- [ ] 数字加 tabular-nums
- [ ] 复核浅色主题（现面板默认深色，浅色主题的 bad case 需专门走查）

### 阶段 3：组件层（3–4 天）
- [ ] 按钮五档 + 五变体 + 四态
- [ ] 图标按钮四变体 + 按压变形
- [ ] Chip / Badge 拆分
- [ ] 文本域重建（**工作量最大**）
- [ ] 进度条 wavy + loading indicator
- [ ] switch / checkbox / segmented button
- [ ] divider / tooltip / list item

### 阶段 4：页面结构（2–3 天）
- [ ] app bar 64px + scrolled 态
- [ ] 导航三形态 + 键盘导航 + hash 路由
- [ ] 内容宽度双轨 + 居中
- [ ] 空态 / 骨架 / 错误态

### 阶段 5：动效（2 天）
- [ ] spring 令牌落地（`linear()` 采样）
- [ ] §7.2 逐项替换
- [ ] stagger 与共享轴转场
- [ ] `prefers-reduced-motion` + 应用内开关

### 阶段 6：交互与可访问性（2 天）
- [ ] `:focus-visible` 全局
- [ ] `<dialog>` 迁移（6 个对话框）
- [ ] skip link / aria-label / aria-live / aria-selected
- [ ] 表单字段级校验
- [ ] 键盘走查（Tab 序、ESC、方向键）

### 阶段 7：适配（1–2 天）
- [ ] 五档 size class 断点
- [ ] 容器查询重写网格
- [ ] `100dvh` / 滚动模型
- [ ] Electron 窗口最小尺寸下调 + 背景色跟随主题
- [ ] 三态主题 + 系统跟随 + 高对比

### 阶段 8：收尾（1 天）
- [ ] `main.js` 行内样式清零
- [ ] 图标/徽标视觉统一
- [ ] README 与新文档
- [ ] 全量回归（§11）

**合计约 13–18 个工作日**（单人）。

---

## 11. 回归与验证

### 11.1 必须通过的既有门禁

```bash
node --check apps/panel/main.js
node --check apps/electron/main.js
node --check apps/electron/preload.js
node scripts/check-panel-auth.js
node scripts/check-router-port.js
node scripts/check-packaging.js
grep -q 'Content-Security-Policy' apps/panel/index.html
! grep -n 'webSecurity:\s*false' apps/electron/main.js
cargo build -p codex-mp-web      # 验证 RustEmbed 仍能编译（新增资源）
```

### 11.2 双链路验证（**关键**）

面板必须在这两条路径下都验证，它们的差异是历史 bug 的高发区：

| 路径 | 加载方式 | 要验证 |
|---|---|---|
| Electron 桌面 | `file://` + `window.electronAPI` | 免密 token、窗口拖拽、窗口控制、字体加载、CSP |
| 浏览器 / 远程 | `http://127.0.0.1:31828` | 登录、401 重登、字体加载、CSP 响应头、缓存头 |

### 11.3 响应式验证矩阵

| 宽度 | 尺寸类 | 导航形态 | 检查 |
|---|---|---|---|
| 380 | Compact | bottom bar | 无横向溢出、按钮可点 |
| 600 | Medium | rail | 网格 2 列 |
| 900 | Expanded | rail | 内容居中 |
| 1220（默认） | Large | drawer | 网格 4 列 |
| 1600 | XL | drawer | 内容不拉伸 |

### 11.4 主题与偏好矩阵

| 维度 | 取值 |
|---|---|
| 主题 | system / light / dark × 系统实际值 |
| 对比度 | standard / high |
| 动效 | 全量 / reduced（OS 与 App 两级独立设置） |
| 缩放 | 100% / 125% / 150% / 200% |

### 11.5 功能回归清单

- [ ] 登录 / 登出 / 401 重登 / 退出后 URL token 被清
- [ ] Router 状态三态显示
- [ ] Desktop 状态 + 安装 / 恢复
- [ ] 账号：保存登录态 / 导入 / 切换并重启 / 重命名 / 删除 / 单条刷新 / 批量刷新
- [ ] 服务商：添加 / 编辑 / 删除（含凭证清除）/ 发现模型 / 导入所选 / 手动添加模型
- [ ] 模型：启用停用 / 编辑名称与上下文 / 删除
- [ ] 同步到 Codex（含 Electron `sync-finished` 事件）
- [ ] 安全设置：密码 / 网页访问开关 / 远程访问开关
- [ ] 提示、确认、输入三类对话框的取消语义

---

## 12. 风险与取舍

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| 1 | **M3 Expressive 无 Web 官方实现**，全部手写 | 与 Compose 实现存在视觉/物理差异；规范更新时需自行跟进 | 本文档把令牌值写死为可核对常量；改动集中在令牌层 |
| 2 | CSS spring 需 `linear()` 采样（Chromium 113+） | Electron 33(130) 与目标浏览器均支持，但采样质量决定手感 | 采样点 ≥ 32，并用 `@keyframes` 兜底 |
| 3 | **文本域重建**牵动 13 处 HTML + 所有表单提交路径 | 是本次最容易引入功能回归的项 | 单独分支；每个表单做完即端到端测一遍 |
| 4 | **`<dialog>` 迁移**改变显隐机制 | 现有 `style.display` 逻辑遍布 `main.js`，漏改会出现"点不开/关不掉" | 统一封装 `openDialog/closeDialog`，禁止直接操作 `display` |
| 5 | **字阶缩小**（body-small 13→12、label-small 12→11） | 中文在小字号下可读性下降 | 中文字形需实测；必要时保留 12/11 的下限并放宽行高 |
| 6 | **Electron 最小宽度下调到 400** | 更多尺寸组合暴露布局缺陷 | 阶段 7 才做，且配合 §11.3 矩阵 |
| 7 | `file://` 下 CSP `font-src` 匹配不确定 | 字体在桌面端静默回退 | 阶段 0 先实测；必要时双写 `'self' file:` |
| 8 | 静态回退返回 200 HTML | 资源路径写错时表现为"字体解码失败"而非 404，难排查 | `lib.rs` L498–515 对静态后缀返回真 404（§9.6 #3） |
| 9 | 行内样式有 179 处（`index.html` 70 + `main.js` 模板 74 + `main.js` `.style.` 35） | 清理不彻底会导致"改了类但覆盖还在" | 阶段 8 用脚本断言清零；CI 加检查 |
| 10 | 动效与 `innerHTML` 全量重渲染冲突 | `refresh*()` 会重建 DOM，入场动画可能反复播放 | 用 `data-animated` 标记，仅在首次渲染播放入场 |
| 11 | 改动量跨 9 个文件、约 3000 行 | 单次 PR 过大不可审 | 严格按 §10 的 8 个阶段分 PR |
| 12 | 项目当前无前端测试 | 回归靠人工 | 至少补 `scripts/check-panel-tokens.js`（§9.8 #6）作为机器门禁 |

---

## 附录 A：令牌对照与迁移速查

### A.1 形状

| 语义 | 旧令牌 | 新令牌 | 值 |
|---|---|---|---|
| 极小 | `--md-shape-corner-extra-small` | `--md-sys-shape-corner-extra-small` | 4 |
| 小 | `--md-shape-corner-small` | `--md-sys-shape-corner-small` | 8 |
| 中 | `--md-shape-corner-medium` | `--md-sys-shape-corner-medium` | 12 |
| 大 | `--md-shape-corner-large` | `--md-sys-shape-corner-large` | 16 |
| 大+ | — | `--md-sys-shape-corner-large-increased` | **20（新）** |
| 特大 | `--md-shape-corner-extra-large` | `--md-sys-shape-corner-extra-large` | 28 |
| 特大+ | — | `--md-sys-shape-corner-extra-large-increased` | **32（新）** |
| 超特大 | — | `--md-sys-shape-corner-extra-extra-large` | **48（新）** |
| 全圆 | `--md-shape-corner-full` | `--md-sys-shape-corner-full` | full |

> 注意：本项目现有令牌前缀是 `--md-shape-corner-*`，而 M3 官方是
> `--md-sys-shape-corner-*`。建议统一为官方命名（属于阶段 1 的机械替换）。

### A.2 运动

| 场景 | 令牌 |
|---|---|
| 按钮/图标按钮按压 | `spring-fast` spatial |
| 开关、chip 选中 | `spring-default` spatial |
| 卡片抬升、进度 | `spring-slow` spatial |
| 颜色/透明度变化 | `spring-*-effects`（不过冲） |
| 视图转场入场 | `emphasized-decelerate` 400ms |
| 视图转场出场 | `emphasized-accelerate` 200ms |
| 对话框入场 | `emphasized` 500ms 或 `spring-default` |
| 短暂工具提示 | `standard` 300ms |

### A.3 字重

| 用途 | 基线 | Emphasized |
|---|---|---|
| Display / Headline / Body | 400 | 500 |
| Title / Label | 500 | 700 |

---

## 附录 B：新增 CSS 类命名表

| 类 | 用途 |
|---|---|
| `.m3-navigation-rail` / `.m3-navigation-bar` | 导航另两形态 |
| `.m3-app-bar--scrolled` | 顶栏滚动态 |
| `.m3-btn-xs/-sm/-md/-lg/-xl` | 按钮五档 |
| `.m3-btn-elevated` | 缺失的第五变体 |
| `.m3-btn-group` / `.m3-split-button` | Expressive 新增 |
| `.m3-fab-menu` / `.m3-fab-menu-item` | Expressive 新增 |
| `.m3-icon-btn--filled/-tonal/-outlined` | 图标按钮变体 |
| `.m3-icon--filled` | 图标填充态 |
| `.m3-chip` / `.m3-chip--success/-warning/-error/-assist/-input` | Chip 体系 |
| `.m3-badge-dot` / `.m3-badge-count` | M3 Badge 语义 |
| `.m3-text-field--filled` / `--outlined` / `--error` / `.m3-text-field-support` | 文本域 |
| `.m3-divider` / `.m3-divider--inset` | 分隔线 |
| `.m3-list` / `.m3-list-item` / `.m3-list-item--selected` | 列表 |
| `.m3-tooltip` | 工具提示 |
| `.m3-segmented-button` / `.m3-segmented-item` | 分段按钮 |
| `.m3-skeleton` | 骨架屏 |
| `.m3-loading-indicator` | Expressive 加载指示器 |
| `.m3-progress--wavy` | 波浪进度 |
| `.m3-dialog--md` / `--lg` / `--danger` | 对话框变体 |
| `.m3-snackbar-action` | Snackbar 动作 |
| `.m3-skip-link` | 跳转链接 |
| `.m3-empty-state` / `.m3-error-state` | 空/错误态 |
| `.m3-quick-grid` | 替换总览页内联 grid |
| `.is-hidden` | 替换行内 `display:none` |
| `.m3e-*`（动画名） | Expressive 动画前缀 |

---

## 附录 C：删除 / 合并项

| 项 | 位置 | 处理 |
|---|---|---|
| `.m3-fab` | `styles.css` L522–546 | 重做为 Expressive FAB 并启用，或删除（当前 0 引用） |
| `.m3-card-outlined` | L570–573 | 保留（2 处使用），但圆角改 medium |
| `.m3-display-small` / `.m3-headline-medium` / `.m3-body-large` | L162/163/167 | 当前 0 引用；保留作为完整字阶的一部分 |
| `.m3-btn-danger` | L490–499 | 合并进 `.m3-btn--danger` 修饰符体系 |
| `[style*="display: none"]` 兜底 | L938–940 | 删除（改用 `<dialog>` 后不再需要） |
| `transition: all` | L435 / L700 / L712 / L815 | 全部替换为显式属性 |
| `--md-motion-duration-long` | L60 | 已声明未使用；被新 duration 体系取代 |
| `linear-gradient(135deg,…)` 品牌图标 | L281 | 删除渐变 |
| 179 处行内样式 | `index.html` 70 + `main.js` 模板 74 + `.style.` 35 | 清零 |
