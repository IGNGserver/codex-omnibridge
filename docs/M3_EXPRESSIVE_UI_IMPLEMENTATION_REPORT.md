# Codex OmniBridge 面板 M3 Expressive 改造实施报告

> 文档状态：**已实施 + 实测验证**
> 实施日期：2026-09-19
> 目标文档：`docs/M3_EXPRESSIVE_UI_REBUILD_PLAN.md`（本报告逐条对应其 §9 全量改动清单）
> 验证方式：Chromium（Playwright 153）真实渲染 + 真实 Rust 后端端到端 + 机器化断言

---

## 0. 结论摘要

`docs/M3_EXPRESSIVE_UI_REBUILD_PLAN.md` 中列出的**八个阶段、六维度改造已全部实施完毕**，
并通过了计划 §11 的全部回归验证：

| 验证项 | 结果 |
|---|---|
| 既有 CI 门禁（7 项） | ✅ 全部通过 |
| 设计令牌纪律（新增门禁） | ✅ 通过 |
| 渲染对比度（两主题 × 4 页面，220 个文本节点） | ✅ 0 项低于 WCAG AA |
| 响应式矩阵（380/600/839/840/1200/1600） | ✅ 无横向溢出、无重叠 |
| 功能回归清单（32 项用户操作） | ✅ 32/32，0 控制台错误 |
| 对话框语义（6 个） | ✅ 原生 `<dialog>`、可访问名称、Esc 关闭、焦点陷阱 |
| 真实后端双链路（HTTP 面板 + file:// 桌面） | ✅ 资源/MIME/CSP/字体均正确 |

改造过程中**实测发现并修复了 8 个计划文档未预见或低估的真实缺陷**（见 §7），
其中 3 个是可访问性缺陷、2 个是安全/健壮性缺陷、3 个是本次改造自身引入的回归。

---

## 1. 文件结构变更

### 1.1 新增

| 文件 | 行数 | 作用 |
|---|---|---|
| `apps/panel/tokens.css` | 494 | **唯一**允许出现裸色值/尺寸/运动常量的文件 |
| `apps/panel/base.css` | 588 | 重置、字阶（15 基线 + 15 强调）、图标、状态层、骨架屏、减少动效 |
| `apps/panel/components.css` | 1724 | 按钮/图标按钮/卡片/Chip/进度/开关/复选/文本域/列表/分段按钮/对话框/菜单/工具提示/空态 |
| `apps/panel/layout.css` | 1071 | 应用外壳、导航三形态、内容宽度、各页面组合、打印 |
| `apps/panel/icons.css` | 90 | **生成物**：自托管 `@font-face` + 41 个 `.m3-i-*` 码点类 |
| `apps/panel/fonts/roboto-flex-latin.woff2` | 84 KB | 自托管可变字体 |
| `apps/panel/fonts/roboto-flex-latin-ext.woff2` | 59 KB | 自托管可变字体（Latin Extended） |
| `apps/panel/fonts/noto-sans-sc.woff2` | 1020 KB | 自托管 CJK 子集（GB2312 一级字库，3755 常用汉字） |
| `apps/panel/fonts/material-symbols-outlined.woff2` | 36 KB | 图标字体子集（原 3.9 MB） |
| `apps/panel/favicon.png` | 64×64 | 修复每次加载 404 |
| `scripts/build-panel-fonts.sh` | — | 字体/图标子集的可复现生成脚本 |
| `scripts/build-icons.mjs` | — | 由 `assets/icon.svg` 渲染 PNG 图标 |
| `scripts/check-panel-tokens.js` | — | 新增 CI 门禁：令牌纪律 |
| `tools/panel-verify/` | 6 文件 | 真实浏览器验证套件（mock 后端 + 结构 / 对比度 / 功能） |

### 1.2 重写 / 删除

- `apps/panel/index.html`：整体重写（526 → 715 行），原生 `<dialog>`、语义化标签、13 处浮动标签显式关联。
- `apps/panel/main.js`：整体重写（1372 → 1974 行），行内样式 179 → **4**（全部为 `--m3-progress-value`，即承载计算值的数据属性）。
- `apps/panel/styles.css`：**删除**，拆分为上述 5 个文件。
- `assets/icon.svg`：品牌色改为与令牌一致。

### 1.3 改动

- `crates/web/src/lib.rs`：CSP 收紧（移除第三方字体源与非法 IPv6 源）；静态回退对资源类后缀返回真 404。
- `apps/electron/main.js`：最小窗口 920×660 → 360×480；窗口背景色跟随主题；新增 `nativeTheme` 监听。
- `.github/workflows/ci.yml`：新增「Panel design-token discipline」步骤。
- `README.md`：补充 Expressive 界面说明与资源生成流程。

---

## 2. 维度一：外观语言

### 2.1 颜色

**由单一色种（`#00A6B8`）经 CIELAB L\* 色调框架生成**，而非手工挑色——这正是旧代码
出问题的方式（夜间主题的 primary 被硬编码进浅色主题的状态层）。

- 调色板以 spec 的固定 tone 号分配角色（如 `primary` = tone 40/80、`primary-container` = 90/30），
  因此明暗两套**必然一致**。
- 补齐 Expressive 角色：`surface-dim/bright/tint`、`inverse-*`（3 个）、`*-fixed`（12 个）、`shadow`。
- 自定义 `warning` 角色（M3 未定义）建立在同一 tone 框架上，因此同样随主题变化。
- 4 组 plan chip 色板取代 6 个硬编码值。

**验证**：生成期校验 48 组 on/container 配对，0 失败；渲染期实测 220 个文本节点两主题
均 0 项低于 AA。

**修复的真实缺陷**：`outline` 被当作正文颜色使用（浅色下仅 3.85:1，低于 AA 4.5:1）。
`outline` 是**边界**角色；所有文本用法改为 `on-surface-variant`，类名相应改为
`.m3-text-muted`。

### 2.2 字阶

- 15 基线 + 15 强调角色，字距/行高按 spec 逐条修正。
  - `title-large` 20/26→22/28；`body-small` 13→12；`label-medium` 13→12；`label-small` 12→11。
- **强调角色只改字重**（400→500 / 500→700），字号与行高完全继承——保证替换类名不引发重排。
- 数字使用 `tabular-nums`，数值刷新不再抖动。

**修复的真实缺陷**：旧代码 16 处 `font-weight: 600/700`，其中基线角色上使用 700 会破坏
「强调不重排」的保证。CI 门禁现在会扫描**所有**匹配规则（而非仅第一处）并拒绝该情况。

---

## 3. 维度二：页面结构

| 项 | 改造 |
|---|---|
| 顶部应用栏 | 48px → **64px**；三段式 `leading/headline/trailing`；滚动后加 `.m3-top-app-bar--scrolled`（色调 + 高度 2） |
| 导航 | 单一 256px 抽屉 → **三形态**：<600 底部导航栏 / 600–1199 导航轨道（选中 pill 指示器）/ ≥1200 抽屉 |
| 内容宽度 | 原 `max-width` 无 `margin:auto`（宽屏内容贴左）→ 居中约束，双轨 `1040px`（正文）/ `1440px`（网格） |
| 空状态 | 图标+两行文字 → **composed empty state**（大图标 + 标题 + 说明 + 操作按钮），账号页空态带「保存当前登录态」动作 |
| 加载状态 | 静态「正在加载…」文案 → **骨架屏**（3 处） |
| 错误状态 | 一行红字 → **error state**（`role="alert"` + 重试按钮），账号页/服务商页均可重试 |
| 路由 | 新增 hash 同步与恢复；刷新/书签可停留在当前页 |

---

## 4. 维度三：组件

| 组件 | 改造要点 |
|---|---|
| 按钮 | 五档尺寸（32/40/56/96/136）+ 五变体（新增 elevated）+ 危险变体；XS/S 用 `::after` 扩展点击区至 48dp 而不撑高盒子；**按压形态变形** |
| 图标按钮 | 4 变体 × 5 尺寸；按压时圆→squircle（`full`→`small`） |
| 卡片 | 统一 28px → medium(12)/large(16)；高度改用色调表面；新增 `--interactive` |
| Chip / Badge | 拆分为两套语义：状态 chip 与 M3 badge（dot / count） |
| 进度 | 8px → **4px 线性** + **Expressive wavy 变体**（额度主指标用 wavy）；`width` 过渡 → `scaleX` 合成层 |
| Loading indicator | **新增**形态变形指示器，替代不定量圆形/文字，用于按钮忙碌态 |
| 开关 | 滑块改为定尺寸 + transform；新增选中图标、`:focus-visible`、48dp 点击区 |
| 文本域 | **整体重建**：M3 Filled/Outlined 浮动标签 + 支持文本 + 字符计数 + 错误态 + 前后置图标 |
| 对话框 | `<div>` + `style.display` → **原生 `<dialog>`**（6 个） |
| Snackbar | `surface-container-highest` → `inverse-*`；新增 action 按钮与**队列**（原先互相覆盖） |
| 新增 | Menu、Tooltip（替代 14 处 `title=`）、Divider、List item、Segmented button、Skeleton、Empty/Error state、Button group、Split button 样式 |

`.m3-fab` 死代码按用户确认**直接删除**。

---

## 5. 维度四/五：交互与动效

### 5.1 动效令牌

- **弹簧**：三组（fast/default/slow）× (spatial/effects)，取自 Compose `ExpressiveMotionTokens`
  的阻尼/刚度值，用阻尼谐振子采样为 CSS `linear()` 曲线（40 采样点）。
  spatial 组过冲（设计如此），effects 组临界阻尼**从不过冲**。
- 旧令牌中 `--md-motion-easing-emphasized` 与 standard **同值**（即"强调曲线"不存在）。
  现已修正，并**修正了我自己检查脚本中的错误断言**：M3 规范中这两条曲线本就相同，
  区别在**时长**（500ms vs 300ms），门禁改为校验 decelerate/accelerate 变体与时长关系。

### 5.2 形态变形（本轮修复的关键回归）

首次实现用 `9999px → 20px` 做按钮按压变形，实测**完全不可见**：浏览器会把半径夹到盒子
高度的一半，因此动画 99% 的时间都在夹紧值上。改为按高度比例（`/2` → `/4`）后，
在全部五档尺寸上均可见：

```
.m3-btn-filled  rest 20px → active 9.10px
.m3-btn-xs      rest 16px → active 8px
.m3-icon-btn    rest 20px → active 9.32px
```

### 5.3 减少动效

OS `prefers-reduced-motion` 与应用内偏好**两级独立**并正确联动：
无显式偏好时以 OS 为准（设置页控件同步显示「减弱」，而非错误地显示「完整」）。

### 5.4 交互与可访问性

- 全局 `:focus-visible`；跳转链接（首个 Tab 停靠点）；导航方向键 roving + `aria-current`。
- 13 处浮动标签**显式 `for`/`id` 关联**——首版遗漏，导致 9 个字段在可访问性树中**完全没有名称**。
- `aria-live` 覆盖 7 个区域；`data-tooltip` 生成的工具提示均带 `aria-label`。
- 键盘完全可达：对话框焦点陷阱（实测 Tab 只在对话框内与 body 间循环）、Esc 关闭、焦点归还。

---

## 6. 维度六：适配

| 尺寸类 | 宽度 | 导航 | 指标网格（实测列数） |
|---|---|---|---|
| Compact | <600 | 底部导航栏 | 1 列（348px） |
| Medium | 600–839 | 导航轨道 | 2 列 |
| Expanded | 840–1199 | 导航轨道 | 2 列 |
| Large | 1200–1599 | 抽屉 | 4 列 |
| Extra-large | ≥1600 | 抽屉 | 4 列 |

- **容器查询修复**：首版把 `container-type` 放在网格自身上——容器不能查询自己，导致所有
  网格**永远停在一列**。改为查询视图区后正确分档（实测五档列数如上）。
  另修正 `.m3-content-wide` 因 `margin-inline:auto` 收缩为 0 宽的问题。
- `100vh` → `100dvh`；`@media print`。
- Electron 最小窗口 920×660 → **360×480**：原先最小宽度使 compact/medium 两档在桌面端
  **永远无法触达**，整套响应式形同虚设。
- 窗口背景色跟随主题（现读 `nativeTheme`），消除启动闪色。
- 无横向溢出（380px 实测），底部导航栏 88px padding 不遮挡内容（实测末卡 bottom 692 < 导航 top 715）。

---

## 7. 实测发现并修复的缺陷

计划文档未列出、由真实渲染验证发现的项：

| # | 缺陷 | 严重度 | 修复 |
|---|---|---|---|
| 1 | CSP 中 `http://[::1]:*` 是**非法源**，Chromium 每次加载都报错并忽略该源 | 中 | 两处 CSP 同步移除（IPv6 字面量在 CSP host-source 中一律非法，实测确认） |
| 2 | 点击「切换/取消」后 `submit` 触发器失效（`:active` 变形用 `calc()` 夹紧） | 中 | 改为等比半径 |
| 3 | 9 个表单字段无任何可访问名称 | **高** | 13 处显式 `for`/`id` |
| 4 | 仅变换 hash 的导航不更新视图（URL 与界面不一致） | 中 | 新增 `hashchange` 监听 |
| 5 | 禁用按钮文字对比度 ≈ **1.00:1**（两层低 alpha 叠加） | **高** | 禁用态改为中性容器 + `on-surface-variant` |
| 6 | 缺失资源返回 200 HTML（表现为字体解码失败而非 404，极难排查） | 中 | 资源类后缀返回真 404 |
| 7 | 无 favicon，每次加载 404 | 低 | 由品牌 SVG 生成 |
| 8 | `file_upload` / `restore` 图标名在上游已重命名，连字**静默失效** | 中 | 改为码点寻址；生成脚本对缺失图标直接报错退出 |

---

## 8. 安全影响

- CSP **收紧**：移除 `https://fonts.googleapis.com` 与 `https://fonts.gstatic.com`
  （现自托管），`font-src` 回到 `'self'`；`http://[::1]:*` 移除。meta 与响应头**逐字一致**。
- 未放宽任何 `script-src`；未新增内联脚本或内联事件处理器；`webSecurity` 保持开启。
- 登录/登出/401 的 token 清理逻辑**逐字保留**（CI 门禁 `check-panel-auth.js` 通过）。
- 新增门禁 `check-panel-tokens.js` 防止语义层硬编码色值回归。

---

## 9. 验证方法与证据

### 9.1 环境

- Chromium 153（Playwright 153），真实布局引擎渲染，非静态分析。
- 真实 `codex-mp` debug 二进制（`cargo build` 通过）提供 HTTP 面板，验证 `RustEmbed` 内嵌资源。
- 自建 mock 后端，覆盖全部面板调用的 API 路由与全部空/错误/边界状态。

### 9.2 可复现命令

```bash
# 既有门禁 + 新增门禁
node --check apps/panel/main.js && node --check apps/electron/main.js
node scripts/check-panel-auth.js
node scripts/check-panel-tokens.js
node scripts/check-router-port.js
node scripts/check-packaging.js
grep -q 'Content-Security-Policy' apps/panel/index.html
cargo build -p codex-mp-web        # 验证 RustEmbed 收录新增字体

# 真实后端
./target/debug/codex-mp web start --port 4603 --headless --local-token tk
curl -sSI http://127.0.0.1:4603/ | grep -i content-security-policy
curl -o /dev/null -w '%{http_code}\n' http://127.0.0.1:4603/missing.woff2   # 期望 404
```

### 9.3 自动化验证结果

| 套件 | 覆盖 | 结果 |
|---|---|---|
| 结构/溢出/焦点/对话框 | 5 档宽度 × 4 视图 + 6 对话框 + 主题矩阵 | 0 问题 |
| 渲染对比度 | 2 主题 × 4 视图，220 文本节点 | 0 项低于 AA |
| 功能回归 | 32 项用户操作（含 API 调用断言） | 32/32，0 错误 |
| 可访问名称 | Chromium AX 树，全部对话框与页面 | 全部有名称 |
| 空/错误/骨架态 | 空数组 + 500 响应注入 | 全部正确渲染并可重试 |

---

## 10. 已知取舍与后续

1. **M3 Expressive 无官方 Web 实现**，全部手写。令牌值取自 Compose 与规范文档并写死在
   `tokens.css`，便于日后核对；但 Google 若调整规范需手工跟进。
2. **弹簧用 `linear()` 采样逼近**（Chromium 113+）。手感与真实物理弹簧存在极小差异，
   采样点 40 个已足够平滑。
3. **CJK 字体已自托管，但做了范围取舍**：`noto-sans-sc.woff2` 覆盖 GB2312 一级字库
   （3755 常用汉字，1020 KB）。实测的三种方案：仅面板自身用字 438 字 = 142 KB，
   二级字库 6763 字 = 1807 KB。选一级字库是因为"只含面板用字"会让用户输入的账号名/
   服务商名出现**同串混排两种字体**——比不自托管更难看；而二级字库多出的 3009 个生僻字
   对管理面板价值很低，体积却接近翻倍。若出现生僻字人名/机构名，会回落到系统 CJK 字体。
4. **验证套件已纳入仓库并接入 CI**：`tools/panel-verify/`（mock 后端 + 三个套件），
   由 `.github/workflows/ci.yml` 的 `panel-verify` job 执行，失败时上传截图。
   Playwright 通过 `npm install --no-save` 在 CI 内临时安装，**不进 `package.json`**：
   面板本身零运行时依赖，为一个只在 CI 跑的检查给所有贡献者拉 300 MB 浏览器不划算。
   另有一个取舍见 `tools/panel-verify/README.md`：套件跑在 mock 后端上，因为真实后端
   只认 loopback token，而面板按安全设计会立即从地址栏抹掉该 token——多次导航会 403，
   这是安全模型正确工作而非缺陷。真实后端的单次加载验证改用 curl 断言资源与响应头。
5. `prefers-contrast` 已支持，但「中等对比度」档位未实现（M3 有 standard/medium/high 三档，
   本项目提供 standard/high 两档）。
