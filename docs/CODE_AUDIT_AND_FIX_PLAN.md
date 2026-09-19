# Codex OmniBridge 代码审计与修复计划

> ⚠️ **本文档的 §11「实施状态」已被后续实测推翻，请先读
> [`docs/VERIFICATION_AND_FIX_PLAN.md`](VERIFICATION_AND_FIX_PLAN.md)。**
>
> 本文档的审计与修复**都在没有 Rust 工具链的机器上完成**（见第 27 行、§11.4），
> 因此 §11.2 声称"已完成"的改动**从未编译过**。后续在具备工具链的环境中实测发现：
> 工作区**无法编译**（6 个类型错误）；8 个坑位中有 3 处"修复"本身是新缺陷
> （其中"已修复 D-01"实际仍在摧毁用户的 `config.toml` 行尾注释）；测试套件有 4 个
> 失败被当作通过；并且**默认配置下 `provider add` 与 Router 的自定义模型路由
> 都会因 keyring-in-async 直接 panic**——即核心功能当时不可用。
>
> 上述问题已在 `VERIFICATION_AND_FIX_PLAN.md` 中修复并逐项实测。
> 本文档的**问题清单（§1–§9）与未完成项（§11.3）仍然有效**，是后续修复的输入。

> 文档状态：**独立代码质量与缺陷审计报告**（静态分析，未经编译验证）
> 审计日期：2026-09-17
> 审计基线：工作区 `416f3bd`（工作树含未提交改动）
> 审计范围：`crates/`（10 个 crate，约 20,400 行 Rust）、`apps/`（Electron + 面板，约 115 KB JS/HTML/CSS）、`installer/`、`scripts/`、`patches/`、`.github/workflows/`

---

## 0. 本文档的定位与边界

本文档**不是**架构合同，也**不是**验收报告。它是对当前工作树的一份**缺陷与代码质量审计**，用于驱动下一轮修复工作。

与既有文档的关系：

| 文档 | 与本文档的关系 |
|---|---|
| `docs/CCSWITCH_MECHANISM_AUDIT_AND_REBUILD_PLAN.md` | 当前**唯一机制合同**，本文档不修改其结论 |
| `docs/codex-integration.md` | 当前集成行为说明，本文档与之基本一致 |
| `docs/DEVELOPMENT_REMEDIATION_PLAN.md` | **已过时且与当前代码直接矛盾**，见 I-01 |
| `README.md` | 存在与代码不符的宣传性描述，见 I-02、I-03 |

**证据分级**：本文档中标注 ✅ 的条目已由审计者本人逐行核对源码确认；标注 ⚠️ 的条目来自静态分析，逻辑链完整但未做运行时验证。所有行号对应当前工作树。

**未执行项（诚实声明）**：

- 本机（Windows）**没有 Rust 工具链**，无法运行 `cargo check` / `cargo test` / `cargo clippy`。所有编译期结论均为源码级推断。
- 无法运行任何安装器、`systemd`、`dbus`、`launchctl`、注册表写入或 Electron 打包流程。
- `cargo test` 声称的 220 个测试**未被实际执行验证**。
- 因此本文档不改变机制合同中任何 `PASS / FAIL / NOT PROVEN` 的既有判定；本文档新增的结论一律视为**静态分析结论**。

---

## 1. 执行摘要

### 1.1 量化基线

| 指标 | 数值 |
|---|---|
| Rust 源文件 / 行数 | 31 文件 / ~20,400 行 |
| 最大 crate | `protocol-bridge` 11 文件 / 9,161 行（占 45%） |
| Cargo.lock 依赖包数 | 340 |
| 测试总数 | 213（`protocol-bridge` 占 161，即 **76%**） |
| 安全关键但测试极少 | `web` 3 个、`cli` 2 个、`credentials` 2 个、`catalog` 4 个 |
| 关闭全部 clippy 的文件 | **8 个 / ~8,000 行**（`#![allow(clippy::all)]`） |
| 生产代码 `unwrap()` | 2 处 |
| 生产代码 `expect()` | 16 处 |
| 未使用依赖 | 8 处（跨 6 个 crate） |
| 安装器重复代码 | Linux/macOS ≈ 55–60% 逐行重复 |

### 1.2 结论概述

当前项目的问题可以归为四类，**彼此独立**：

1. **安全缺陷（8 项 P0）**——Web 面板会把 ChatGPT OAuth `refresh_token` 返回给浏览器、密码哈希不是 KDF、远程访问为明文 HTTP、Electron 主动关闭同源策略、面板存在 DOM XSS。这些问题在"开启外网/局域网访问"这一**已宣传功能**下会被直接放大。
2. **数据破坏与不可恢复状态（6 项 P1）**——`config.toml` 注释被摧毁、安装/恢复流程存在崩溃窗口导致用户永久卡死、Windows 安装器会误杀自身进程。
3. **契约与文档矛盾（3 项）**——`auth.json` 账号切换功能与 README/机制合同的"绝不触碰 auth.json"承诺**直接对立**；一份 920 行的整改文档描述的是已被推翻的架构。
4. **臃肿与冗余（量化见第 7 节）**——7,447 行移植代码与 `third_party/` 7,379 行近乎逐行重复；8,000 行关闭了所有 clippy 检查；安装器三平台四份脚本大量复制且已经分叉。

**没有任何一项问题需要破坏机制合同来修复。**

---

## 2. 问题总览

| ID | 问题 | 严重度 | 类别 | 证据 |
|---|---|---|---|---|
| S-01 | Web 面板向浏览器返回 OAuth `refresh_token` | **P0** | 安全 | `web/src/lib.rs:1011,1068,1092` |
| S-02 | 密码哈希为单轮加盐 SHA-256，注释谎称 PBKDF2 | **P0** | 安全 | `web/src/lib.rs:49-62` |
| S-03 | 登录接口无速率限制 / 锁定 | **P0** | 安全 | `web/src/lib.rs:385-419` |
| S-04 | 远程访问为明文 HTTP，密码与令牌裸奔 | **P0** | 安全 | `web/src/lib.rs:1221-1228` |
| S-05 | `local_token` 免密通道无 loopback 限制 | **P0** | 安全 | `web/src/lib.rs:243-262` |
| S-06 | Electron `webSecurity:false` + 无导航守卫 + `openExternal` 未校验 | **P0** | 安全 | `electron/main.js:158-164,204-207` |
| S-07 | 面板 DOM XSS：`planClass` 未转义 → 可窃取 `localToken` | **P0** | 安全 | `panel/main.js:464-475` |
| S-08 | CORS 全放通 + 静态资源无 CSP | **P0** | 安全 | `web/src/lib.rs:215` |
| I-01 | 920 行整改文档描述已被推翻的架构，仍写"必须停止编码" | **P0** | 契约 | `docs/DEVELOPMENT_REMEDIATION_PLAN.md:120-137` |
| I-02 | `auth.json` 账号切换功能与 README 承诺直接矛盾 | **P0** | 契约 | `manager/src/account.rs` 全文、`README.md:15` |
| I-03 | 同一工作区三套不同的应用目录标识 | **P1** | 契约 | `core:567` / `credentials:66` / `credentials:19` |
| D-01 | `config.toml` 注释与格式被 `toml::to_string_pretty` 摧毁 | **P1** | 数据破坏 | `integration/src/lib.rs:235` |
| D-02 | 先写 config 后写 manifest → 崩溃窗口 → 永久不可恢复 | **P1** | 数据破坏 | `integration/src/lib.rs:236-260` |
| D-03 | registry 保存失败后 manifest 与 config 不一致 → 安装卡死 | **P1** | 数据破坏 | `integration/src/lib.rs:260-278` |
| D-04 | integration v2 `restore` 非幂等 → 永久死锁 | **P1** | 数据破坏 | `integration/src/lib.rs:445-484` |
| D-05 | `desktop::restore` 先删 runtime 再恢复入口 → 崩溃留下损坏 launcher | **P1** | 数据破坏 | `desktop/src/lib.rs:536-563` |
| D-06 | `desktop::install` 崩溃窗口 → 环境变量永久重定向且无 manifest | **P1** | 数据破坏 | `desktop/src/lib.rs:451-488` |
| B-01 | Windows 进程匹配子串 `codex` → 误杀 `codex-mp` 自身 | **P1** | Bug | `manager/src/account.rs:972-985` |
| B-02 | Router HTTP 客户端无超时且跟随重定向 | **P1** | 安全/Bug | `router/src/lib.rs:112` |
| B-03 | Router 启动无跨进程锁 → 双实例竞争 | **P1** | Bug | `manager/src/lib.rs:588-609` |
| B-04 | generation 过期的健康 Router 被遗弃为孤儿进程 | **P1** | Bug | `manager/src/lib.rs:588-594` |
| B-05 | `RouterSupervisor` 无 `kill_on_drop`，仅处理 Ctrl-C | **P1** | Bug | `manager/src/lib.rs:596-609,756-765` |
| B-06 | `authorize()` 在未配置 token 时**失败开放** | **P1** | 安全 | `router/src/lib.rs:1377` |
| B-07 | `chrono_iso_now()` 返回 Unix 秒并写入 `auth.json` | **P1** | Bug | `manager/src/account.rs:769-773` |
| B-08 | capability token 明文写入 `config.toml` | **P2** | 安全 | `integration/src/lib.rs:373-377` |
| B-09 | Windows 端点令牌文件权限从不校验 | **P2** | 安全 | `router/src/lib.rs:245-256` |
| B-10 | 状态文件读-改-写无跨进程锁 → 丢失更新 | **P2** | Bug | `account.rs:191-220`、`core:545-563` |
| B-11 | `restore` 按 manifest 记录路径删文件，无包含性校验 | **P2** | 安全 | `integration/src/lib.rs:476-483` |
| B-12 | `history_routes` 无界增长（内存泄漏） | **P2** | 性能 | `router/src/lib.rs:87` |
| B-13 | Web 面板忽略 `--registry`，同步到默认目录 | **P1** | Bug | `web/src/lib.rs:639,692` |
| B-14 | `desktop/install` 要求 JSON body，前端不发 → 按钮必失败 | **P2** | Bug | `web/src/lib.rs:545` vs `panel/main.js:1249` |
| B-15 | 会话 token 永不过期、不可撤销、无界增长 | **P2** | 安全 | `web/src/lib.rs:97,410,472` |
| P-01 | 自定义模型继承官方 `shell_type`/`model_messages`，与注释矛盾 | **P2** | Bug | `catalog/src/lib.rs:236-241` |
| P-02 | `resolve_route` 与真实路由逻辑分叉，且失败开放 | **P2** | 死代码 | `router/src/lib.rs:336-368` |
| X-01 | Responses→Chat：`null` 值被原样透传给严格上游 | **P2** | Bug | `ported/transform_codex_chat.rs:295-330` |
| X-02 | 结构化输出 `text.format` 被静默丢弃 | **P2** | Bug | 同上 `:30-45` |
| X-03 | `finish_reason=content_filter` 被映射为 `completed` | **P2** | Bug | 同上 `:2040-2045` |
| X-04 | 畸形 SSE 帧被静默丢弃，无日志无错误事件 | **P2** | Bug | `ported/streaming_codex_chat.rs:868-871` |
| X-05 | history hydration 会把 `input` 由 object 变形为 array | **P2** | Bug | `ported/codex_chat_history.rs:190-194` |
| X-06 | `"error": null` 被当作致命上游错误 | **P2** | Bug | `ported/streaming_codex_chat.rs:873` |
| L-01 | Linux `CODEX_MP_INSTALL_SERVICE=1` **完全不可用**且留下半安装 | **P0** | Bug | `install-linux.sh:99-104` + `install-router-service.sh:13` |
| L-02 | Electron `.deb` 的 `prerm` 为死代码 → 卸载不还原配置 | **P0** | Bug | `package.json:80-84`；`installer/linux/deb/prerm` 无引用 |
| L-03 | systemd unit 硬编码路径，且 registry 路径与 CLI 默认不一致 | **P1** | Bug | `installer/codex-mp-router.service:9-10` |
| L-04 | PowerShell 安装器吞掉 service 安装失败仍报成功 | **P1** | Bug | `install-windows.ps1:115-120,156-157` |
| L-05 | NSIS 卸载器忽略失败、仍打印 "uninstalled cleanly" | **P1** | Bug | `uninstaller.nsh:15-21` |
| L-06 | 三个卸载器在 CLI 缺失时中止 → 半安装无法清理 | **P1** | Bug | `uninstall-*.sh:10-13`、`uninstall-windows.ps1:29-31` |
| L-07 | `apply.sh` 从不校验 `SHA256SUMS` | **P1** | 安全 | `apply.sh:44-57` |
| L-08 | macOS 无 manifest 卸载残留 8 个文件（Linux 会删） | **P1** | Bug | `uninstall-macos.sh:27-29` |
| C-01 | 无 push / PR CI，测试仅在手动 release 时运行 | **P0** | CI | `.github/workflows/release.yml:3-4` |
| C-02 | CI "smoke" 仅做语法检查，无法发现任何真实失败 | **P1** | CI | `release.yml:126-148` |
| C-03 | 发布前校验不含 portable 归档，且非草稿立即公开 | **P2** | CI | `release.yml:368-371,397` |
| C-04 | Actions 未固定到 commit SHA，`rust-toolchain@stable` 非不可变 | **P2** | CI | `release.yml:115` 等 |
| C-05 | 4 目标矩阵无 Rust/Node 缓存 | **P2** | CI | `release.yml:110-124,156-160` |
| C-06 | 验收脚本 `stock-omnibridge-e2e.py`(578 行) 从不被 CI 调用 | **P2** | CI | 无 `.github` 引用 |
| H-01 | 死文件 `installer/windows/setup.iss`(71 行) 与错误的 `RELEASE.md` | **P2** | 卫生 | `setup.iss` 零引用；`RELEASE.md:19,37` |
| H-02 | 已放弃的 Tauri 面板残留 5 处引用 | **P2** | 卫生 | `.gitignore:2`、`router:1442,1951`、2 份文档 |
| H-03 | MIT 归属声明未随 `.deb`/`.AppImage`/NSIS 包分发 | **P2** | 合规 | `package.json:26-41`、`build-deb.sh:29-58` |
| H-04 | `untitled-project/`（空 git 仓库）、空目录 `tests/` | **P3** | 卫生 | 未跟踪 |
| H-05 | 无 `.gitattributes` + `core.autocrlf=true` → 16 个 shell 脚本被 CRLF 污染 | **P1** | 卫生 | 实测：16 文件显示为"已修改"，diff 为 0 |
| H-06 | 文档中固化的开发者绝对路径 `/home/lvziw/...` | **P3** | 卫生 | `patches/.../README.md:44,50` 等 |

---

## 3. P0 —— 阻断级问题

### S-01 ✅ Web 面板向浏览器返回 ChatGPT OAuth `refresh_token`

`crates/web/src/lib.rs:1011` / `:1068` / `:1092` 三处直接把 `ManagedAccount` 序列化返回：

```rust
Ok(acc) => Json(acc).into_response(),          // :1011 api_capture_account
```

`ManagedAccount`（`crates/manager/src/account.rs:116`）内嵌 `pub tokens: AccountTokens`，后者包含 `id_token` / `access_token` / **`refresh_token`**（`account.rs:46-54`）。代码库**已经定义**了不含令牌的 `AccountSummary`（`account.rs:131-140`）但只用于 `list_accounts`。

危害：`refresh_token` 泄漏 = ChatGPT 账号完全接管。当 `allow_remote` 绑定 `0.0.0.0` 且传输为明文 HTTP 时（S-04），该令牌会以明文跨越局域网。

**修复**：三个 handler 一律改返回 `AccountSummary`；新增一个显式断言测试，扫描所有 `/api/v1/accounts*` 响应体确认不含 `refresh_token`/`access_token`/`id_token` 字段。

---

### S-02 ✅ 密码哈希不是 KDF，且注释宣称是 PBKDF2

`crates/web/src/lib.rs:49-62`：

```rust
/// PBKDF2-like salted SHA-256 password hash helper   // ← 注释与实现不符
pub fn hash_password(password: &str) -> String {
    let salt = Uuid::new_v4().simple().to_string();
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(password.as_bytes());
    ...
}
```

单轮加盐 SHA-256，**无迭代/工作因子**。同样的错误描述复制在 `crates/core/src/lib.rs:23`（`/// PBKDF2-HMAC-SHA256 hash formatted as salt$hash`）。GPU 可对弱口令做数十亿次/秒爆破，加盐无济于事。

**修复**：改用 `argon2`（首选）或 `pbkdf2` + ≥600k 迭代；`stored_hash` 增加算法前缀（如 `argon2id$...`）并保留对旧格式的**一次性迁移**校验路径；同步修正两处注释。

---

### S-03 ✅ 登录接口无速率限制或锁定

`crates/web/src/lib.rs:385-419` 的 `api_security_login` 校验后立即返回，全文件无尝试计数、延时或来源追踪。与 S-02（弱哈希）和 S-04（明文 HTTP）组合，局域网内任意主机可无限次在线爆破。

**修复**：按来源 IP + 全局两级滑动窗口限流（如 5 次/分钟、20 次/小时）；连续失败指数退避；失败计数与成功登录日志（不含密码）。

---

### S-04 ✅ 远程/局域网访问为明文 HTTP

`crates/web/src/lib.rs:1221-1228` 绑定 `Ipv4Addr::UNSPECIFIED`（0.0.0.0）+ 裸 `TcpListener`，`axum::serve`（`:1268`）无 TLS。登录 POST 明文发送密码（`:385-388`），S-01 明文发送令牌。`web/src/lib.rs:1250-1254` 仅提示"外网/局域网访问: [已开启]"，**无任何安全警告**；`README.md:37-40` 还主动推荐用 `http://<主机IP>:31828` 远程访问。

**修复**（三选一，需产品决策）：
1. 强制自签名 TLS 并在首次访问时提示指纹校验；
2. 明确将远程访问降级为"仅限可信内网"，在 CLI 输出与 README 中加入阻塞性安全警告；
3. 远程模式改为通过 SSH 隧道 / Tailscale 等外部方案，`--enable` 时打印具体命令。

无论选哪种，`web remote --enable` 都应要求已设置密码（当前是否强制需在实现时确认）。

---

### S-05 ✅ `local_token` 免密通道没有 loopback 限制

`crates/web/src/lib.rs:243-262`：

```rust
// 1. 如果请求携带了本地桌面特权 Local Token，无条件免密放行
if let Some(ref tok) = token {
    if constant_time_equal(tok.as_bytes(), state.local_token.as_bytes()) {
        return next.run(request).await;
    }
}
```

注释写"本地桌面特权"，但**没有任何来源 IP / loopback 判断**。该分支位于 `web_enabled` 与密码门禁**之前**。当绑定 0.0.0.0 时，任何拿到该 token 的局域网客户端均可完全绕过密码。

`crates/web/src/lib.rs:1231` 还有一处：`local_token.unwrap_or_else(|| Uuid::new_v4().to_string())` 不校验非空，因此 `--local-token ""` 会让**空 Bearer** 通过任意请求。

**修复**：
1. 校验 `ConnectInfo<SocketAddr>` 为 loopback，否则直接忽略该 token；
2. 启动时拒绝空 `local_token`；
3. token 增加轮换（每次进程启动重新生成，见 S-05 相关项 H-06 的 `local_token` 持久化问题）。

---

### S-06 ✅ Electron 关闭同源策略、缺少导航守卫、`openExternal` 未校验

`apps/electron/main.js:158-164`：

```js
webPreferences: {
  nodeIntegration: false,
  contextIsolation: true,
  sandbox: false,
  webSecurity: false,      // ← 关闭同源策略
}
```

同时：
- `main.js` 只注册了 `setWindowOpenHandler`（`:204`），**没有 `will-navigate` / `will-redirect` 守卫**。`preload.js:4-16` 在**每个导航后**都会重新注入特权 API 与 secret，因此任何主框架导航（`target="_self"` 链接或服务端 302）都能拿到 `electronAPI.localToken` 并调用 `syncCodex()`（spawn Rust 二进制）与 `quitApp()`。
- `main.js:204-207` 把任意 URL 直接交给 `shell.openExternal(url)`，无 scheme/host 白名单 → `file://`、`smb://`、自定义 scheme 全部放行。
- `apps/panel/index.html` **无 CSP**，Rust 静态处理器也只设 `CONTENT_TYPE`（`web/src/lib.rs:314-322`），因此没有第二道防线来压制 S-07。

**修复**：
1. 去掉 `webSecurity: false`（若因 `file://` 加载需要放宽，改为用自定义协议或本地 HTTP server 提供页面）；
2. 增加 `will-navigate` / `will-redirect` 守卫，只允许预期 origin；
3. `openExternal` 增加 `http`/`https` 白名单，其余拒绝；
4. 在 `index.html` 与 Rust 静态响应中同时加严格 CSP（`default-src 'self'; script-src 'self'; object-src 'none'`）；
5. `ipcMain` 的 `get-local-token-sync` 校验 `event.senderFrame.url` 是否为本应用 origin。

---

### S-07 ✅ 面板 DOM XSS —— 从"导入账号凭据"到命令执行的完整链路

`apps/panel/main.js:464-475`：

```js
const plan = acc.plan_type || "plus";
const planClass = plan.toLowerCase();
...
<span class="m3-plan-badge ${planClass}">${escapeHtml(plan)}</span>
```

**同一行里文本被转义、类名没有**。`plan_type` 来自导入 ID-token 的 `https://api.openai.com/auth.chatgpt_plan_type` 声明（`crates/manager/src/account.rs:806-809`），用户可通过面板的"导入账号凭据"粘贴框（`panel/main.js:1082-1096`）直接提供。

构造 `plan_type = 'x"><img src=x onerror="fetch(...)">'` 即可突破 class 属性执行脚本，进而在 Electron 渲染进程中读取 `electronAPI.localToken` 并调用 `syncCodex()`。结合 S-06（无 CSP、无导航守卫、`webSecurity:false`），这是**可直接利用的完整 RCE 链路**。

另有一处同类隐患：`panel/main.js:489,498,504` 的 `data-id="${acc.id}"` 未转义（当前因 id 是 UUID 而安全，但相邻的 `data-name` 已转义，容易被未来改动传染）。

**修复**：`planClass` 改为白名单映射（`plus/pro/business/...` → 固定 class，未知值用 `default`）；所有属性插值统一走一个 `escapeAttr()`；把 `escapeHtml` 升级为同时转义 `"` 与 `'` 的统一函数。**此项应作为 P0 第一优先修复。**

---

### S-08 ✅ CORS 全放通

`crates/web/src/lib.rs:215`：`.layer(CorsLayer::permissive())` 覆盖整个管理 API，含 `security/login` 与全部 `accounts` 路由。任何网页都可跨域读取响应；无 origin 白名单，无 CSRF token。

**修复**：改为固定 origin 白名单（`http://localhost:<port>` 与 Electron 的 `file://`/自定义协议 origin）；对状态变更接口增加 CSRF token 或要求非简单请求头。

---

### I-01 ✅ 920 行整改文档描述的是已被推翻的架构

`docs/DEVELOPMENT_REMEDIATION_PLAN.md` 通篇把以下行为定义为**必须停止编码的停止条件**（第 120-137 行的 I1、第 768-770 行）：

```toml
model_provider = "multiprovider"   # 不得
model_provider = "omnibridge"      # ← 当前代码实际写入的值
```

而 `crates/integration/src/lib.rs:355-358` 正是写入 `model_provider = "omnibridge"`，`:367-379` 正是创建 `[model_providers.omnibridge]`。

该文档第 3-5 行自称"保留为历史整改记录"，但第 7 行同时声称自己是"审计后续实施基线"，第 875 行仍下达"下一步不得回退到 Catalog-only"之类的指令。**一份 920 行的文档同时自称历史记录与当前基线，且其核心不变量与当前代码相反，对任何新加入的开发者都是主动误导。**

**修复**：把该文档移到 `docs/archive/`，在文件头只保留"本文档描述 2026-09 之前的 patched-Core 架构，已被 CCSWITCH 合同取代，仅作历史参考"，删除所有祈使句与其失效的验收清单；在 `docs/CCSWITCH_MECHANISM_AUDIT_AND_REBUILD_PLAN.md` 顶部加一行指向归档位置。另外修正 `README.md` 中与该文档冲突的描述。

---

### I-02 ✅ `auth.json` 账号切换功能与 README／机制合同直接对立

代码事实（✅ 已核对）：

- `crates/manager/src/account.rs` 全文 **1,027 行**专用于账号管理，`pub mod account;`（`manager/src/lib.rs:1`）并对外重导出。
- `:185-186` 定义 `auth_json_path()` → `codex_home.join("auth.json")`
- `:224-233` `read_active_auth()` 读取 `auth.json`
- `:512-530` `switch_to_account()` 备份并**覆写** `auth.json`（`atomic_replace(&temp_path, &auth_path)`）
- `:334-356` `capture_current_auth()` 把当前登录态抓取为托管账号
- `:17-19` 直连 `https://auth.openai.com/oauth/token` 刷新 OAuth 令牌、`https://chatgpt.com/backend-api/wham/usage` 查询额度
- `crates/web/src/lib.rs:95,132,1350` 把它挂进 Web 面板，`:1011/:1068/:1092` 暴露为 HTTP 接口

文档事实：

- `README.md:15`：**"项目不读取、不修改 OAuth 或 `auth.json`"**
- `README.md:137,172`：重复同一承诺
- `docs/CCSWITCH_MECHANISM_AUDIT_AND_REBUILD_PLAN.md:112`：**"OmniBridge 不创建、不刷新、不写入 `auth.json`"**
- 同文档 `:850`：**"不读取、打印、复制或提交用户 `auth.json` 内容"**

这是本项目**最核心的产品承诺**（README 第一段"绝不影响官方账号登录态"）与**已交付功能**之间的正面冲突。此外该功能本身还有独立缺陷：明文 `accounts.json` 保存 `refresh_token`（`account.rs:116`）、每次切换留下 `auth.bak-switch-*` 凭证副本且永不清理（`:512-516`）、`terminate_pid` 无结果校验、Windows 进程匹配误杀（B-01）。

**修复（需产品决策，二选一）**：

- **方案 A（推荐，与现有文档一致）**：删除 `manager::account` 与全部 `accounts` Web 接口。该功能与项目定位（"不碰官方登录态"）无关，且是当前最大的安全面。删除后 `README` 的承诺成立，S-01 随之一并消失。
- **方案 B**：保留功能，但必须同时：改写 README/机制合同的承诺措辞、令牌**只存 keyring 不存明文 JSON**、接口只返回 `AccountSummary`、清理 `auth.bak-switch-*`、修正 B-01/B-07。

无论选哪个，**在决策前不应发布任何版本**——当前状态下 README 的承诺是失实的。

---

### L-01 ✅ Linux `CODEX_MP_INSTALL_SERVICE=1` 完全不可用，并留下半安装状态

`installer/install-linux.sh:99-104`：

```bash
install -m 0755 "${ROUTER_SERVICE_INSTALLER}"   "${INSTALL_DIR}/codex-mp-router-service-install"
install -m 0755 "${ROUTER_SERVICE_UNINSTALLER}" "${INSTALL_DIR}/codex-mp-router-service-uninstall"
# ↑ 只复制了两个脚本，没有复制 codex-mp-router.service

if [[ "${CODEX_MP_INSTALL_SERVICE:-0}" == "1" ]]; then
  CODEX_MP_ENABLE_SERVICE="${CODEX_MP_ENABLE_SERVICE:-0}" \
    "${INSTALL_DIR}/codex-mp-router-service-install"
fi
```

`installer/install-router-service.sh:4-13`：

```bash
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
...
install -m 0644 "${SCRIPT_DIR}/codex-mp-router.service" "${UNIT_PATH}"
```

脚本被复制到 `${INSTALL_DIR}` 后，`SCRIPT_DIR == ${INSTALL_DIR}`，而该目录**不含 `codex-mp-router.service`**（对比 `install-macos.sh:82` 确实安装了 `dev.codex-multiprovider.router.plist`）。

后果：`install: cannot stat ...` → 因 `set -euo pipefail`（`install-linux.sh:2`）在**第 104 行中止**，此时二进制已复制（97-100）但 manifest **尚未写入**（130+）→ 半安装且不可被卸载器识别。

`README.md:131-133` 明确文档化了这条命令（远端常驻 Router 工作流），因此这不是边缘路径。

**修复**：在 `install-linux.sh` 中一并安装 `codex-mp-router.service`（与 macOS 对齐），或让 `install-router-service.sh` 支持 `CODEX_MP_UNIT_SOURCE` 覆盖；同时把整个安装流程改为"先在临时目录完成全部校验与准备，最后一步原子提交 + 写 manifest"。

---

### L-02 ✅ Electron `.deb` 的 `prerm` 是死代码，导致 `apt remove` 不还原配置

`package.json:80-84` 只配置了 post-removal：

```json
"deb": {
  "packageCategory": "utils",
  "priority": "optional",
  "afterRemove": "installer/linux/deb/postrm"
}
```

**没有 `beforeRemove`/prerm 钩子**。仓库中确实存在 `installer/linux/deb/prerm`（它会停 Router 并调用 `codex-mp uninstall`），但全仓库仅 `scripts/build-deb.sh:60-77` 提到 `prerm`——那是**另一套打包脚本自己生成**的 prerm，不是这个文件。也就是说：

- CI 实际发布的 `.deb`（由 electron-builder 构建，`release.yml:175`）**没有 prerm**；
- `installer/linux/deb/prerm` 从未被任何构建流程使用。

后果：`apt remove` / `dpkg -r` 后 Router 继续运行，用户的 `~/.codex/config.toml` 仍指向已被删除的 `model_provider = "omnibridge"`，与 `README.md:195-205` 的"零污染承诺"直接冲突。

**修复**：在 `package.json` 的 `deb` 段增加 `"beforeRemove": "installer/linux/deb/prerm"`；删除 `scripts/build-deb.sh` 中重复生成的 prerm/postrm；把 `installer/linux/deb/prerm` 的 root/`SUDO_USER` 两个分支补齐（现存版本比 `build-deb.sh` 生成的更完整，应以仓库版本为准）。

---

### C-01 ✅ 没有任何 push / PR CI

`.github/workflows/` 下只有一个文件 `release.yml`，其触发条件是：

```yaml
on:
  workflow_dispatch:
```

因此 `cargo fmt` / `cargo test`（`release.yml:119-120`）、全部安装器 smoke 检查**只在维护者手动发起 release 时运行**——也就是说，一个已经破坏 main 的提交会在被推送后长期无人发现，直到准备发版。结合本项目当前的质量状况（8,000 行关闭 clippy、`web` 只有 3 个测试），这是所有其他问题的**放大器**。

**修复**：新增 `.github/workflows/ci.yml`，在 `push` 与 `pull_request` 上运行：
`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`node --check apps/**/*.js`、`bash -n installer/*.sh scripts/*.sh`、PowerShell parser 检查、以及一个"禁止新增 `#![allow(clippy::all)]`"的门禁脚本。

---

## 4. P1 —— 高优先级

### D-01 ✅ `config.toml` 的注释与格式被摧毁

`crates/integration/src/lib.rs:234-236`：

```rust
apply_omnibridge_config(&mut config, &applied_value, &router_base_url, &capability)?;
let updated_config = toml::to_string_pretty(&config)?;     // ← 全量重序列化
if let Err(error) = write_config(&paths.codex_config, &updated_config) {
```

`config` 是 `toml::Value`（纯数据模型，**无注释表示能力**），因此整份 `config.toml` 会被重写：所有用户注释、空行、对齐、以及无关 table（`[profiles.*]`、`[mcp_servers.*]`、`[history]` 等）的原始格式**全部丢失**。

`docs/codex-integration.md:203` 明确承诺"保留 LF/CRLF"，`DEVELOPMENT_REMEDIATION_PLAN.md` 声称只动 `model_catalog_json`。现有 CRLF 测试（`integration/src/lib.rs:920`）只覆盖基于行的 `set_root_string_field`，**从未覆盖这条路径**。

**修复**：改用 `toml_edit` 做保留格式的定点修改（只改 `model_provider`、`model_catalog_json` 与 `[model_providers.omnibridge]` 三项）；补一条 fixture 测试：输入带注释与多 table 的 config，断言注释与无关 table 字节级不变。

---

### D-02 ✅ 先写 config、后写 manifest → 崩溃窗口 → 用户永久不可恢复

`crates/integration/src/lib.rs:236`（写 config，此时 `model_provider` 已变为 `omnibridge`）与 `:260`（写 manifest）之间存在崩溃窗口：

```
236: write_config(...)        // config.toml 已被劫持
     ← 进程在此被 kill / 断电
260: save_manifest(...)        // manifest 从未写出
```

后果链：config 指向 `omnibridge` 但**无 manifest** → 后续 `build_and_install` 在 `:213-216` 命中 `UserChangedProvider` 直接报错；`restore` 在 `:519-525` 返回 `Ok(false)`（无 manifest 视为未安装）。**用户没有任何工具路径可以回到官方配置。**

同理 `desktop::install` 在 `:453` 设置 `CODEX_CLI_PATH`（写注册表/`launchctl`）后、`:488` 写 manifest 前也存在同构窗口（D-06）。

**修复**：统一改为"**先写意图记录，再改状态，最后标记完成**"的两阶段提交：
1. 先写 manifest 的 `pending` 状态（含 config 原文快照）；
2. 再改 config/catalog；
3. 成功后把 manifest 置为 `applied`。
启动时若发现 `pending` manifest，则自动回滚到快照。这样任意崩溃点都可恢复。

---

### D-03 ✅ registry 保存失败后 manifest 与 config 不一致 → 安装卡死

`crates/integration/src/lib.rs:260-278`：manifest 已在 `:260` 写出，随后 `:274` 的 `effective_registry.save()` 失败时只回滚 config 与 catalog（`:275-276`），**不移除 manifest**。

结果：manifest 声称 `applied_value`，而 config/catalog 已还原 → 下次 `build_and_install` 报 `UserChangedManagedField`、`restore` 报 `UserChangedProvider`，**安装流程彻底锁死**，只能手工删 manifest。

**修复**：把 `effective_registry.save()` 移到 manifest 写出**之前**；或在失败分支中加入 `fs::remove_file(&paths.manifest)`（并检查其结果）。

---

### D-04 ✅ integration v2 `restore` 非幂等 → 永久死锁

`crates/integration/src/lib.rs:445-484`：

```rust
if root_string(&config, &manifest.managed_field).as_deref() != Some(manifest.applied_value.as_str())
   || root_string(&config, &manifest.managed_provider).as_deref() != Some(manifest.applied_provider.as_str())
   || !has_omnibridge_provider(&config) {
    return Err(IntegrationError::UserChangedProvider);      // :445-452
}
...
write_config(&manifest.config_path, &toml::to_string_pretty(&config)?)?;      // :475
if manifest.catalog_path.exists() { fs::remove_file(&manifest.catalog_path)?; }   // :476 ← 可能失败
...
fs::remove_file(&paths.manifest)?;                          // :484 manifest 最后删
```

若 `:475` 成功（config 已还原为原值）但 `:476` 或后续步骤失败，函数提前返回且 **manifest 仍存在**。下次 `restore` 时 `:445` 的守卫发现 config 已是原值（≠ `applied_value`）→ 判定为"用户改动" → 永久报错。

v1 分支有显式恢复路径（`:490-497` 的 `already_restored`），**v2 分支没有**。测试 `restore_retries_after_config_was_restored_but_cleanup_failed` 只覆盖 v1。

**修复**：在 v2 分支加入同样的 `already_restored` 判定（config 等于 `original_value` 即视为已还原，继续执行清理）；并把清理步骤改为幂等（不存在即跳过，失败可重入）。

---

### D-05 ✅ `desktop::restore` 先删 runtime 再恢复入口 → 崩溃留下损坏的 Codex

`crates/desktop/src/lib.rs:536-563`：

```rust
if let Err(error) = remove_owned_runtime(&manifest, manifest_path) { ... }        // :539 runtime 已删
if !external_launcher {
    copy_file_atomic(&manifest.original_backup, &paths.entrypoint, true)?;        // :546 入口在之后才恢复
}
...
fs::remove_file(&manifest.original_backup)?;                                       // :562
fs::remove_file(manifest_path)?;                                                   // :563
```

Linux 路径下 runtime 先被删除，若在 `:539` 与 `:546` 之间崩溃，用户得到的是一个**指向已不存在 runtime 的 launcher**——官方 `codex` 入口直接损坏。任何在 `:548-562` 的失败都会在 launcher/config/runtime 已删除的情况下保留 manifest，下次 `restore` 因 `verify_launcher_binary`（`:937-945`）或 `verify_backup`（`:810-821`）失败而**无法继续**。

**修复**：调整顺序为"先恢复入口 → 再删 runtime → 最后删 manifest"；每步幂等；`remove_owned_runtime` 改为在入口恢复成功后才执行。

---

### D-06 ✅ `desktop::install` 崩溃窗口 → 环境变量永久重定向且无 manifest

`crates/desktop/src/lib.rs:451-453` 与 `:488`：`set_desktop_override(&launcher_path)?` 把 `CODEX_CLI_PATH` 写入 `HKCU\Environment` / `launchctl setenv`，但清除它的回滚代码**只在 `save_manifest` 返回 `Err` 时执行**。若进程在两者之间崩溃，用户环境被永久重定向到 `codex-mp` launcher，且**没有 manifest**，`restore_if_present`（`:567-574`）返回 `false`，没有任何东西会清理它。Linux 路径同理（`:462` 覆写真实 `codex` 入口）。

**修复**：同 D-02 的两阶段提交；或在写入 override **之前**先写一个最小化的"恢复令牌"文件，任何后续启动都优先检查它。

另见 L-03/`:1145-1184`：macOS 的持久化 override 写了一个 LaunchAgent plist 却**从不 `launchctl load/bootstrap`**，因此"持久化"实际不生效；且 `target.display()` 未做 XML 转义（路径含 `&`/`<` 会生成损坏的 plist），写入是非原子的且错误被 `let _ =` 吞掉。

---

### B-01 ✅ Windows 进程匹配用子串 `codex` → 误杀 `codex-mp` 自身

`crates/manager/src/account.rs:972-985`：

```rust
let img = fields[0].to_lowercase();
if img.contains("codex") {                                  // 匹配 codex.exe / codex-mp.exe / codex-mp-panel.exe ...
    if let Ok(pid) = fields[1].parse::<u32>() { pids.push(pid); }
}
```

`restart_codex_processes`（`:721-725`）随后对这些 PID 执行 `taskkill /F`。对比：Linux 路径要求 cmdline 含 `app-server`（`:952-953`），macOS 用 `pgrep -f "codex.*app-server"`（`:994`）——**只有 Windows 没有收窄条件**。

该路径可由 Web API 触发（`web/src/lib.rs:1096,1155`），即面板上点"切换账号并重启 Codex"会强杀 `codex-mp` 自身。

**修复**：Windows 匹配改为精确镜像名比对（`codex.exe`、`ChatGPT.exe`）**并**校验 cmdline 含 `app-server`，与 Linux/macOS 语义对齐；先排除自身 PID 与父进程 PID。

---

### B-02 ✅ Router 的 HTTP 客户端无超时且跟随重定向

`crates/router/src/lib.rs:112`：`http_client: Client::new()`。

reqwest 默认**跟随最多 10 次重定向**（含跨 scheme：https→http、任意 host），且**无任何默认超时**（无 connect / read / total）。上游 URL 由 `join_url(&provider.base_url, path)`（`:825-832`）拼出，而 `normalize_base_url`（`crates/core/src/lib.rs:741-767`）只做字符串级 scheme/host 校验，**从不解析并固定 IP**。

后果：
1. 恶意/被控的上游返回 `302 Location: http://169.254.169.254/...` 或 `http://127.0.0.1:<port>/...` 即可把本地 Router 变成 SSRF 跳板；
2. 上游挂起时请求任务无限占用，Router 无主动截止时间。

注意 `manager/src/account.rs:170-171` 构造客户端时**有** 15 秒超时，说明团队知道这个模式。

**修复**：`Client::builder().redirect(reqwest::redirect::Policy::none()).connect_timeout(..).timeout(..).build()`；对 provider `base_url` 在 `normalize_base_url` 中增加解析后 IP 校验（拒绝 loopback/私有段/metadata 段，除非显式允许）。

---

### B-03 ✅ Router 启动无跨进程锁 → 双实例竞争

`crates/manager/src/lib.rs:588-609`：

```rust
if let Ok(endpoint) = load_router_endpoint(&self.endpoint_file)
    && self.endpoint_is_healthy(&endpoint).await { self.reused_endpoint = true; return Ok(endpoint); }
remove_stale_endpoint(&self.endpoint_file)?;
let mut child = Command::new(&self.executable); ... .arg("--port").arg("0") ...
```

没有锁文件 / `create_new` 保护。两个并发 CLI 进程都会观察到"无健康端点"，都执行 `remove_stale_endpoint`，都以 `--port 0`（随机端口）启动。随后双方在 `write_router_endpoint`（`router/src/lib.rs:284-293`）与退出清理（`:420-422`）中互相覆盖/删除端点文件，**输掉的一方成为无法被发现的孤儿进程**。

**修复**：用 `OpenOptions::new().create_new(true)` 抢锁文件（含 PID 与启动时间，用于陈旧锁回收）。

---

### B-04 ✅ generation 过期的健康 Router 被遗弃为孤儿

`crates/manager/src/lib.rs:588-594`：`endpoint_is_healthy`（`:630-656`）在检测到运行中 Router 的 `registry_generation` 与当前 registry 不一致时返回 `false`。此时 `start()` **删除该 Router 的端点文件**并启动**第二个** Router。第一个进程不是本进程的 child（不在 supervisor 的 `child` 字段中），**永远不会被 shutdown**，继续占用其端口。

即：**每次 registry 变更都可能泄漏一个孤儿 Router 进程。**

**修复**：识别出 generation 过期时，应先通过其 `/admin/shutdown` 优雅停机（或记录 PID 后终止），确认退出后再启动新实例。

---

### B-05 ✅ `RouterSupervisor` 无 `kill_on_drop`，且只处理 Ctrl-C

`crates/manager/src/lib.rs:596-609`：

```rust
let mut child = Command::new(&self.executable);      // ← 没有 .kill_on_drop(true)
```

`:756-765` 的 `Drop` 手动 `start_kill()`，但 `crates/cli/src/main.rs:963` 的 `run_manager` **只 await `tokio::signal::ctrl_c()`**。因此 `SIGTERM`、Windows 控制台关闭、panic-abort 都会跳过 `Drop`，而 `kill_on_drop` 从未设置 → Router 作为孤儿继续持有端口与端点文件。另外 `Drop` 中的 `start_kill()` 之后没有 `wait()`，在长驻宿主（Web server 路径）上会累积僵尸进程。

**修复**：`.kill_on_drop(true)`；把信号处理扩展到 `SIGTERM`（Unix）与控制台事件（Windows）；`Drop` 中补 `wait()`。

---

### B-06 ✅ `authorize()` 在未配置 token 时失败**开放**

`crates/router/src/lib.rs:1377`：

```rust
let expected = state.capability_token.as_deref()?;      // None ⇒ 返回 None ⇒ 视为已授权
```

`RouterState::new()`（`:92`）是 `pub` 的，产出一个**无认证**状态；所有 handler 都通过 `authorize` 短路，因此直接 `app(state)`（或任何未来的挂载路径）会暴露 `/admin/reload`、`/admin/shutdown`、`/responses` 等全部端点。当前只有 `serve()` 强制校验 token（`:396-398`），仓库内调用方都走 `with_capability_token`，所以这是**潜在陷阱而非现实绕过**——但守卫属于 `authorize`，不属于 `serve`。

**修复**：`authorize` 在 `capability_token.is_none()` 时返回 500/503 并记录错误；把 `RouterState::new()` 改为 `pub(crate)` 或直接删除。

---

### B-07 ✅ `chrono_iso_now()` 返回 Unix 秒并被写进 `auth.json`

`crates/manager/src/account.rs:769-773`：

```rust
fn chrono_iso_now() -> String {
    let secs = now_secs();
    format!("{secs}")        // 例如 "1789413197" —— 不是 ISO-8601
}
```

用于 `switch_to_account` 的 `last_refresh`（`:508`）与 `refresh_account_token`（`:612`）。因为 `switch_to_account` 会**覆写 `auth.json`**，它会用 `"1789413197"` 覆盖 Codex 自己的 `last_refresh` 字段，破坏官方的刷新簿记。函数名与行为完全不符。

**修复**：改用 `time`/`chrono` 输出 RFC3339；若决定保留账号功能（I-02 方案 B），补一条断言 `last_refresh` 可被 RFC3339 解析的测试。

---

### B-13 ✅ Web 面板忽略 `--registry`，同步到默认目录

`crates/web/src/lib.rs:639`（`api_desktop_install`）与 `:692`（`api_catalog_sync`）都使用：

```rust
let integration_paths = IntegrationPaths::default();
```

而 CLI 使用 `IntegrationPaths::for_registry(registry_path)`（`crates/cli/src/main.rs:873`）。`default()` 的 `config_dir` 由 `default_registry_path().parent()` 推导（`integration/src/lib.rs:66-78`），`for_registry` 则由实际 registry 推导（`:82-100`）。

后果：`codex-mp --registry /custom/providers.json web start` 后，在面板点击"同步到 Codex"会把 `models.json`/manifest/`config.toml` 写到**默认目录**，CLI 与面板状态永久不同步。

**修复**：`WebState` 保存 `IntegrationPaths`（由实际 registry 推导），替换这两处 `default()`。

---

### L-03 ✅ systemd unit 硬编码路径，且 registry 路径与 CLI 默认不一致

`installer/codex-mp-router.service:9-10`：

```ini
Environment=CODEX_MP_REGISTRY=%h/.config/codexmultiprovider/providers.json
ExecStart=%h/.local/bin/codex-mp --registry %h/.config/codexmultiprovider/providers.json router --port 8787 ...
```

- `install-router-service.sh` **从不替换该模板**（对比 macOS 在 `install-router-service-macos.sh:39-44` 用 `sed` 替换 `__CLI_PATH__`/`__REGISTRY_PATH__`）。因此设置了 `CODEX_MP_INSTALL_DIR`/`CODEX_MP_REGISTRY`（CI 本身就设置，见 `release.yml:194-196`）时，unit 指向错误的二进制与 registry。
- 该 registry 路径 `dev.codex-multiprovider` 与 CLI 默认（`crates/core/src/lib.rs:566-570`）**不一致**，并且与 **credentials** 的 `ProjectDirs::from("dev","codex","codexmultiprovider")`（`crates/credentials/src/lib.rs:66`）产生混淆。macOS/Windows 用的是 `dev.codex-multiprovider.Codex MultiProvider`。
- 因为 registry 缺失会被当作空 registry 加载（`core/src/lib.rs:335-337` → `Ok(Self::empty(path))`），服务会"健康"启动但**什么都不路由**。

**修复**：让 `install-router-service.sh` 像 macOS 一样在安装时替换占位符；统一三个平台的应用目录标识（见 I-03）。

---

### L-04 ✅ PowerShell 安装器吞掉 service 安装失败仍报成功

`installer/install-windows.ps1:115-120`：

```powershell
& powershell -NoProfile -ExecutionPolicy Bypass -File $serviceInstallerScript -InstallDir $InstallDir -CliBinary $cliDestination
```

同文件的其他原生命令调用都检查 `$LASTEXITCODE`（`:48`、`:65`、`:147`），**唯独这一处不检查**。`$ErrorActionPreference = "Stop"`（`:10`）对原生进程无效。随后 `:156-157` 打印"installed ..."并以 0 退出。CI smoke 从不传 `-InstallRouterService`（`release.yml:253`），因此无人发现。

**修复**：补 `$LASTEXITCODE` 检查并在失败时 `throw`；CI 增加该开关的覆盖。

---

### L-05 ✅ NSIS 卸载器忽略失败仍打印 "uninstalled cleanly"

`installer/windows/uninstaller.nsh:15-21`：

```nsis
nsExec::ExecToLog '"$INSTDIR\bin\codex-mp.exe" uninstall'
Goto DoneRestore
DoneRestore:
  DetailPrint "Codex OmniBridge uninstalled cleanly."
```

`nsExec::*` 会把退出码压栈，此处**从未 Pop 检查**，因此配置还原失败也会报"cleanly"并继续删除应用。另外 `:11` 的 `resources\bin` 分支是死代码——`package.json:36-41` 把二进制放在 `{app}\bin\codex-mp.exe`。

**修复**：`Pop $0` 后判断，失败时中止卸载并给出可操作的错误提示。

---

### L-06 ✅ 三个卸载器在 CLI 缺失时中止 → 半安装无法清理

`installer/uninstall-linux.sh:10-13`、`uninstall-macos.sh:10-13`、`uninstall-windows.ps1:29-31`：

```bash
if [[ ! -x "${CLI_BIN}" ]]; then
  echo "error: ${CLI_BIN} is missing; refusing to remove Codex integration" >&2
  exit 1
fi
```

基于 manifest 的清理（`uninstall-linux.sh:20-26`）被完全跳过。而在 Windows 上更糟：`uninstall-windows.ps1:12-14` **已经注销了计划任务**才抛错。

**修复**：CLI 缺失时降级为"直接按 manifest 删文件"（这正是 manifest 存在的意义），只跳过需要 CLI 执行的 `codex-mp uninstall`；把 Windows 的任务注销移到校验之后。

---

### L-07 ✅ `apply.sh` 从不校验 `SHA256SUMS`

`patches/codex/73a1148c.../apply.sh:44-57` 从 `git apply --reverse --check` 直接走到 `git apply`，**没有摘要校验**。`manifest.json:6` 宣称 `"sha256_file": "SHA256SUMS"`，`SHA256SUMS` 也确实固定了 patch 的摘要，但全仓库**没有任何脚本执行 `sha256sum -c`**。

同时 `SHA256SUMS:1` 内的路径是仓库根相对路径，因此它只在从仓库根执行时有效，而 `build-patched-codex.sh` 会从任意 CWD 调用 `apply.sh`。

**修复**：`apply.sh` 开头执行 `sha256sum -c --ignore-missing`（先 `cd` 到仓库根或把路径改为相对脚本目录）；CI 增加一步验证。

---

### L-08 ✅ macOS 无 manifest 卸载残留 8 个文件

`installer/uninstall-macos.sh:27-29`：

```bash
else
  rm -f -- "${CLI_BIN}" "${UNINSTALLER}"
fi
```

对比 `uninstall-linux.sh:27-35` 会删除 service 脚本、plist 与全部 5 个 stock-Codex 工件。macOS 还**从不删除** `dev.codex-multiprovider.router.plist`（由 `install-macos.sh:82` 安装）。属于第 7 节所述"多平台脚本已分叉"的实例。

**修复**：抽出一个共享的清理函数清单（见第 7.3 节），两平台共用同一文件列表。

---

### H-05 ✅ 无 `.gitattributes` + `core.autocrlf=true` → shell 脚本被 CRLF 污染

实测（本审计环境）：

```
$ git status --short
 M installer/install-linux.sh
 M installer/install-macos.sh
 ... （共 16 个文件，全部为 .sh / .ps1 / .nsh / .py / .py）
$ git diff --stat
 16 files changed, 0 insertions(+), 0 deletions(-)
$ git config --get core.autocrlf
 true
$ Test-Path .gitattributes
 False
```

16 个文件显示为"已修改"但 diff 为 0 行——纯粹是 **LF→CRLF 行尾污染**。提交这 16 个文件会把 CRLF 写入仓库，而其中绝大多数是 **POSIX shell 脚本**（`install-linux.sh`、`install-macos.sh`、`scripts/*.sh`、`installer/linux/deb/*`）。CRLF 的 shell 脚本在 Linux 上会以 `\r: command not found` / `syntax error near unexpected token` 失败——**这是本项目安装路径上一个必然触发的严重缺陷**。

**修复**：
1. 新增 `.gitattributes`：
   ```
   * text=auto eol=lf
   *.sh text eol=lf
   *.ps1 text eol=crlf
   *.nsh text eol=crlf
   *.bat text eol=crlf
   ```
2. `git add --renormalize .` 后提交；
3. 重新核心化工作树的行尾（`git rm --cached -r . && git reset --hard`）。

---

### C-02 ✅ CI "smoke" 只能发现语法错误

`release.yml:126-134` 是一串 `bash -n`（仅语法）；`:144-148` 只解析 PowerShell（`ParseFile`）而**从不执行** `install-windows.ps1`；`:254`、`:257` 的 `if (-not $?)` 检查的是**上一条语句**的状态而非退出码。CI 从不覆盖 `-InstallRouterService`、`CODEX_MP_INSTALL_SERVICE`、`CODEX_MP_INSTALL_DESKTOP` 或 `.deb`/NSIS 的卸载钩子——这正是 L-01、L-02、L-04 长期不可见的根本原因。

**修复**：在容器中真实执行 Linux 安装/卸载往返（含 `CODEX_MP_INSTALL_SERVICE=1`）；Windows/macOS 至少覆盖参数解析与 manifest 往返；对 `.deb` 做 `dpkg -i` + `dpkg -r` 后断言 `config.toml` 已还原。

---

## 5. P2 —— 中优先级

### 协议转换（`protocol-bridge`）

- **X-01 ✅/⚠️** `null` 值透传。`ported/transform_codex_chat.rs:295-330` 用 `body.get(k)` 判断存在性，而 `Value::Null` 也是 `Some`，于是 `"max_tokens": null`、`"temperature": null` 等被发给上游。严格的 OpenAI 兼容网关（vLLM、企业网关）会因数值字段为 `null` 而拒绝。**修复**：所有透传字段统一用 `.is_some_and(|v| !v.is_null())` 过滤。
- **X-02 ✅** 结构化输出丢失。Responses 的 `text.format`（JSON Schema）**从未映射**；被透传的 `response_format` 在 Responses API 中并不存在，因此始终是 no-op。`include` / `store` / `truncation` / `prompt_cache_key` / `reasoning.summary` 同样被静默丢弃。**修复**：补 `text.format` → `response_format` 的显式映射。
- **X-03 ✅** `ported/transform_codex_chat.rs:2040-2045`：`response_status_from_finish_reason` 只识别 `"length"`，其余（含 `content_filter`）一律返回 `"completed"`，且不设 `incomplete_details`。被策略过滤的回合会被上报为正常完成。**修复**：补 `content_filter` → `incomplete` + `incomplete_details.reason`。
- **X-04 ✅** `ported/streaming_codex_chat.rs:868-871`：`serde_json::from_str(&data)` 失败时 `continue`，**无日志、无错误事件**。截断的上游帧会静默丢内容而该回合仍标记完成。**修复**：至少 `warn!` 并发出 `response.failed`。同类模式见 `ported/codex_chat_history.rs:430`、`ported/codex_responses_sse.rs:23`。
- **X-05 ⚠️** `ported/codex_chat_history.rs:190-194`：hydration 会把 `input` 由 object 变形为 array；且 `is_empty_value`（`codex_chat_common.rs:203-211`）把合法的空 `arguments: {}` / `output: []` 当作"缺失"从而从缓存覆盖。
- **X-06 ✅** `ported/streaming_codex_chat.rs:873`：`chunk.get("error").is_some()` 对显式 `null` 也为真 → 因良性字段中止整个流。
- **X-07 ⚠️** `ported/transform_codex_chat.rs:560-571`：`collapse_system_messages_to_head` 的合并分支由 `msg.get("content").and_then(|v| v.as_str())` 守卫，**content 为 parts 数组的系统消息不会被合并**，仍会留在对话中部——而这正是该函数存在的原因（注释自述 MiniMax 会拒绝）。
- **X-08 ⚠️** `ported/transform_codex_chat.rs:735-759` 与 `:704-719` 是同一"工具输出媒体处理"的两条实现路径，且前者在回退分支中规范化的是**未变换**的 `item`（`:758`）。
- **X-09 ⚠️** `"n"` 在透传列表中（`:35`）但只转换 `choices[0]`（`:1536`、`streaming_codex_chat.rs:140`）→ `n > 1` 时静默丢弃额外结果。
- **X-10 ⚠️** 5 个重叠的 pending-reasoning 合并函数（`:1059, :1079, :956, :1103, :1170`）；`append_responses_item_as_chat_message` 是 210 行嵌套 match（`:669-880`），其中 `:817-846` 与 `:847-876` 两个分支重复同一段 flush/emit 序列。
- **X-11 ⚠️** `crates/protocol-bridge/src/lib.rs:144` 把响应中的 `model` 覆盖为上游模型名，Codex 看到的不再是自己请求的 logical id。**需确认是否为有意设计**。

### 其他

- **B-08 ✅** capability token 以明文写入 `config.toml` 的 `http_headers.x-codex-omnibridge-token`（`integration/src/lib.rs:373-377`）。该 token 可授权全部 Router 端点，而 `config.toml` 常被用户复制/粘贴/分享，且在重新 `sync` 时**不轮换**（`:396-410`）。**修复**：改用端点文件（0600）传递，或至少在每次 sync 轮换。
- **B-09 ✅** `router/src/lib.rs:245-256` 的"文件对其他用户可读"检查是 `#[cfg(unix)]` 独占，Windows 上会盲目信任一个任何本地用户可读的 capability 文件。**修复**：Windows 走 ACL 校验。
- **B-10 ✅** registry / accounts / manifest 三处状态文件都是 `load → mutate → atomic_replace`，**无跨进程锁**（`account.rs:191-220`、`core:545-563`、`integration:553-573`）。原子重命名避免了损坏，但**无法避免丢失更新**。**修复**：统一加文件锁。
- **B-11 ✅** `integration/src/lib.rs:476-483` 按 manifest 记录路径 `fs::remove_file`，**无包含性校验**（`desktop` 模块有，见 `:823-854`）。被篡改的 manifest 可删除任意文件。**修复**：复用 desktop 的路径校验。
- **B-12 ✅** `router/src/lib.rs:87` 的 `history_routes: HashMap<String,String>` 只增不删（`:135,892,921` 插入，`:139` 读取），生命周期内每个 response id 一条，**无 TTL 无上限**。**修复**：LRU 或按条数上限裁剪。
- **B-14 ✅** `web/src/lib.rs:545` 用 `Json<Option<DesktopInstallReq>>`，但 axum 的 `Json` 提取器**在 `Option` 生效前就会拒绝空 body**；前端 `panel/main.js:1249` 恰恰不发 body → 设置页"安装适配"按钮必然失败。**修复**：改为 `Option<Json<DesktopInstallReq>>`。
- **B-15 ✅** `web/src/lib.rs:97` 的 `sessions: HashSet<String>` 只插入（`:410,:472`）从不删除——**令牌永不过期、无法撤销、无界增长**；前端"登出"只清 localStorage（`panel/main.js:998-1002`）。**修复**：加 TTL 与显式注销接口。
- **P-01 ✅** `catalog/src/lib.rs:166-169` 的 `custom_entry` 克隆了整个官方模板却**从不清除** `shell_type`（如 `shell_command`）与 `model_messages`，因此每个自定义模型都会宣传官方的 shell 工具与官方系统指令——而 `:236-241` 的注释明确写着 "Function tools are not advertised until..."。`supports_parallel_tool_calls` 也因 `ModelCapabilities::default().tools == true`（`core:110-119`）默认为 `true`。**这是能力伪装**，与 `DEVELOPMENT_REMEDIATION_PLAN.md` 第 625-649 行 F5 的要求直接冲突。**修复**：改用显式字段 allowlist 而非整模板克隆。
- **P-02 ✅** `router/src/lib.rs:336-368` 的 `pub fn resolve_route` **只被单测调用**（`:1551,:1556`），真实路由在 `:632-694` 内联重写。且 `resolve_route` 的 `Err(_) => RouteClass::Custom`（`:359-367`）会把无法解析的模型误标为 Custom——真实路径正确地返回 404，所以这是**语义已分叉的死代码**。**修复**：删除 `resolve_route`，或让真实路径复用它。
- **B-16 ⚠️** `credentials/src/lib.rs:161-165` 的 `with_file_fallback` 只是调用 `new()`（与自己的文档注释矛盾），且全仓库无调用者；文件后端实际由环境变量 `CODEX_MP_SECRET_BACKEND=file` 控制（`:167-169`）。
- **B-17 ⚠️** `catalog/src/lib.rs:93-96` 的 `catalog_schema_fingerprint` 注释声称"so a future Codex binary cannot silently consume an unreviewed shape"，但 `integration` 只写不读（`:227,257`）——**宣称的守卫不存在**。
- **B-18 ⚠️** `credentials/src/lib.rs:251,261` 的 `delete()` 吞掉失败（`let _ =`），删除失败仍返回 `Ok(())` → 调用方误以为密钥已移除。
- **B-19 ⚠️** `credentials/src/lib.rs:182-197` 支持 `CODEX_MP_KEY_<REFERENCE>` 环境变量静默提供 API key，绕过 keyring，且**未文档化**——环境注入可替换上游凭据。

---

## 6. P3 —— 低优先级与清理项

| 项 | 位置 | 说明 |
|---|---|---|
| 死代码 | `router/src/lib.rs:336-368` | `resolve_route` 仅测试调用（见 P-02） |
| 死代码 | `credentials/src/lib.rs:161-165` | `with_file_fallback` 空壳且无调用者 |
| 死代码 | `catalog/src/lib.rs:296-301` | `default_catalog_path` 无调用者 |
| 死代码 | `integration/src/lib.rs:601-613` | `set_root_string_field` 仅测试调用 |
| 死代码 | `protocol-bridge/src/lib.rs:124,154,212,321` | `chat_to_responses` / `chat_sse_to_responses` / `record_responses_sse_stream` / `error_response` 均被 `_with_*` 变体取代 |
| 死代码 | `protocol-bridge/src/lib.rs:43,45` | `BridgeError::MissingChoice` / `Unsupported` 从未构造，但 `router:1278,1281` 仍在匹配 |
| 死代码 | `protocol-bridge/src/tool_media.rs:17,25-33,184,195` | `TOOL_RESULT_MEDIA_ATTACHED_MARKER`、`ToolMediaScope::{ImagesOnly,InlineImagesOnly}`、`strip_media_from_tool_value`、`tool_output_contains_media` 均仅测试引用 |
| 死代码 | `protocol-bridge/src/json_canonical.rs:8,92` | `canonicalize_value` / `short_value_hash` 无生产调用者 |
| 死代码 | `cli/src/tray.rs:17-27` | `TrayHandle::stop` 从未调用（CLI 存为 `_tray`） |
| 死代码 | `web/src/lib.rs:1181,104` | `run_web_server` / `WebState::new` 仅测试使用 |
| 死代码 | `electron/main.js:125-144` | `waitForBackend` 定义后从未调用（连带 `http` import） |
| 死代码 | `electron/main.js:1` | `dialog` 导入后未使用 |
| 死代码 | `electron/preload.js:15` | `getStatus` 调用的 `get-status` 通道**从未注册**，调用必抛 |
| 死代码 | `electron/preload.js:13-15` | `quitApp` / `syncCodex` / `getStatus` 前端从未使用，仅扩大攻击面 |
| 死代码 | `panel/main.js:1285-1295` | `waitForToken` 轮询：`localToken` 由 `sendSync` 同步设置，循环永不执行 |
| 死代码 | `panel/main.js:501` | `data-name` 属性写入后无人查询 |
| 死代码 | `panel/index.html:487` | `id="prompt-dialog-confirm"` 从未被查询 |
| 死 CSS | `panel/styles.css:522-546,162,163,167` | `.m3-fab`、`.m3-display-small`、`.m3-headline-medium`、`.m3-body-large` 无引用 |
| 死配置 | `.gitignore:2` | `/apps/panel/src-tauri/target`——该目录不存在 |
| 死文件 | `installer/windows/setup.iss` | 71 行 Inno Setup 脚本，**零引用**；CI 用 NSIS（`release.yml:224`） |
| 死引用 | `router/src/lib.rs:1442,1951` | 仍允许/测试 `tauri` scheme 与 `tauri://localhost` origin |
| 死文件 | `installer/linux/deb/prerm` | 未被任何构建流程使用（见 L-02） |
| 死分支 | `uninstaller.nsh:11` | `resources\bin` 分支不可达（二进制在 `{app}\bin`） |
| 冗余校验 | `cli/src/main.rs:258-259` vs `:1391-1393` | clap 的 `conflicts_with` 已拒绝，手工重复检查 |
| 死元数据 | `integration/src/lib.rs:135,137,141,143` | `original_provider_present` / `provider_table_present` / `catalog_schema_fingerprint` / `official_model_count` 只写不读 |
| 死字段 | `protocol-bridge/src/lib.rs:28,59` | `BridgeContext::generation` 与 `CodexChatReasoningConfig::output_format` 只写不读 |
| 文档漂移 | `docs/DEVELOPMENT_REMEDIATION_PLAN.md:686,690,700` | 仍声称 Tauri 相关检查 PASS |
| 文档漂移 | `docs/RELEASE.md:19,37` | 声称 Windows 用 Inno Setup 生成 `*-setup.exe`，实际是 NSIS |
| 文档漂移 | `patches/.../README.md:44,50` 等 | 固化 `/home/lvziw/...` 开发者绝对路径 |
| 构建产物 | `dist/` | 7 个陈旧构建产物 + 4 个中间目录（已 gitignore，但影响"干净检出"假设） |
| 未跟踪垃圾 | `untitled-project/`、`tests/` | 前者是只有一个空 `.git` 的目录；后者是空目录 |
| 版本双源 | `apps/electron/package.json:3` | `0.1.0` 与根 `package.json:3` 重复，CI 只 bump 根 → 每次发布都漂移 |
| 环境标识冲突 | `credentials/src/lib.rs:19` | `DEFAULT_SERVICE = "dev.codex-multiprovider"` 与 `:66` 的 `ProjectDirs::from("dev","codex","codexmultiprovider")` 不一致（见 I-03） |

---

## 7. 臃肿与冗余代码清单

### 7.1 最大的单项冗余：7,447 行移植代码 vs 7,379 行 vendored 源码

| 路径 | 行数 |
|---|---|
| `crates/protocol-bridge/src/ported/transform_codex_chat.rs` | 4,721 |
| `third_party/cc-switch/1d5d90f4.../proxy/providers/transform_codex_chat.rs` | 4,708 |
| `crates/protocol-bridge/src/ported/streaming_codex_chat.rs` | 1,309 |
| `third_party/.../streaming_codex_chat.rs` | 1,299 |
| `crates/protocol-bridge/src/ported/codex_chat_history.rs` | 809 / 792 |
| `crates/protocol-bridge/src/ported/codex_responses_sse.rs` | 366 / 361 |
| `crates/protocol-bridge/src/ported/codex_chat_common.rs` | 210 / 209 |
| **合计** | **7,447 / 7,379** |

`THIRD_PARTY_NOTICES.md:9-16` 解释了 `third_party/` 的存在理由（MIT 归属 + 源码出处保留），这是**正当的合规做法**，不应删除。但它带来两个真实成本：

1. **任何上游修复都必须手工双向同步**——目前没有任何机制记录"本地相对上游改了什么"。差异分析显示本地版本每文件仅多出 1–13 行，说明改动很小但完全未文档化。
2. `protocol-bridge` 因此占全部 Rust 代码的 **45%**，而它是唯一一个把 8,000 行代码的 clippy 全部关闭的 crate。

**建议**：保留 `third_party/`，但把本地改动抽成一个可复核的 `.patch`（像 `patches/codex/` 那样），并在 `THIRD_PARTY_NOTICES.md` 追加"本地改动摘要 + 重新移植步骤"。这样 7,379 行的重复从"不可验证的副本"变成"可审计的基线"。

### 7.2 关闭的 lint 覆盖（~8,000 行）

8 个文件首行是 `#![allow(clippy::all)]`：

```
crates/protocol-bridge/src/json_canonical.rs:1
crates/protocol-bridge/src/ported/codex_chat_common.rs:1
crates/protocol-bridge/src/ported/codex_chat_history.rs:1
crates/protocol-bridge/src/ported/codex_responses_sse.rs:1
crates/protocol-bridge/src/ported/streaming_codex_chat.rs:1
crates/protocol-bridge/src/ported/transform_codex_chat.rs:1
crates/protocol-bridge/src/sse.rs:1
crates/protocol-bridge/src/tool_media.rs:1
```

这直接架空了 `DEVELOPMENT_REMEDIATION_PLAN.md:688` 记录的 `cargo clippy --workspace --all-targets --all-features -- -D warnings PASS`——该"PASS"对这 8,000 行毫无意义，且它屏蔽的正是能发现第 5 节 X-01/X-06/X-09 的正确性 lint。

**建议**：先移除 `#![allow(clippy::all)]`，记录实际告警数，再逐条处理或以**精确到 lint 名**的 `#[allow]` 局部豁免（禁止 crate 级 blanket）。

### 7.3 安装器重复（已经分叉）

| 文件 | 行数 | 重复内容 |
|---|---|---|
| `installer/install-linux.sh` | 163 | `:70-159` 与 macOS `:51-149` 近乎逐行相同 |
| `installer/install-macos.sh` | 149 | 同上，≈ 90 行 / 55–60% 重复 |
| `installer/uninstall-linux.sh` | 38 | 与 macOS 版共享 ~25/30 有效行 |
| `installer/uninstall-macos.sh` | 31 | 同上 |

同一份"5 个 stock-Codex 工件"清单被复制 **6 次**（`install-linux.sh:73-78,:108-113`、`install-macos.sh:54-59,:91-96`、`install-windows.ps1:69-75,:123-129`），manifest 写入循环 3 次且**守卫规则各不相同**。

**已经产生的实际分叉**（不是理论风险）：

| 分叉 | 后果 |
|---|---|
| Linux 复制 service 脚本但不复制 unit；macOS 复制 plist | L-01（Linux 服务安装完全不可用） |
| macOS 无 manifest 卸载只删 2 个文件；Linux 删 8 个 | L-08（残留） |
| Linux 无 `if [[ -f ]]` 守卫；macOS 有 | L-10（最小包安装直接中止） |
| `installer/linux/deb/prerm` 有 `else` 分支；`build-deb.sh` 生成的没有 | L-02 关联：root 卸载静默无操作 |
| Linux 用 `dev.codex-multiprovider`；macOS/Windows 用 `dev.codex-multiprovider.Codex MultiProvider` | L-03（服务加载空 registry） |

**建议**：抽出单一数据源（如 `installer/artifacts.txt` 列出 5 个工件、`installer/lib.sh` 提供共享函数），三平台脚本只保留平台特有分支；或改用 `cargo-dist` 之类的成熟方案。

### 7.4 未使用依赖（8 处）

已通过逐 crate grep 核实（生产代码中零引用）：

| crate | 依赖 | 备注 |
|---|---|---|
| `router` | `async-trait` | 0 引用 |
| `router` | `tokio-util` | 0 引用 |
| `router` | `anyhow` | 0 引用 |
| `router` | `async-stream` | 仅在 `protocol-bridge` 使用 |
| `router` | `http` | 只用 `axum::http` |
| `router` | `tower` | 仅 `#[cfg(test)]` 使用 → 应移到 `dev-dependencies` |
| `web` | `tower`、`tracing`、`bytes` | 本 crate 内 0 引用 |
| `web` | `codex-mp-catalog`、`codex-mp-credentials` | 仅 cli/integration/manager/router 使用 |
| `catalog` | `anyhow` | 0 引用 |
| `core` | `toml` | 0 引用 |

`cargo-udeps` 或 `cargo machete` 可自动化此检查并纳入 CI。

### 7.5 重复实现（会漂移的安全关键代码）

| 重复项 | 位置 |
|---|---|
| 私有文件权限设置 | `core/src/lib.rs:808-856` vs `credentials/src/lib.rs:21-63`（近乎逐字复制，含相同的 `whoami.exe`+`icacls.exe` 逻辑） |
| `remove_stale_endpoint` | `manager/src/lib.rs:767-773` vs `router/src/lib.rs:1502-1508` |
| "按可执行文件查找进程" | `account.rs:931-1011`（三平台） vs `desktop/src/lib.rs:1454-1633`（三平台）——语义不一致，B-01 正源于此 |
| `paths_equal*` | `desktop/src/lib.rs:1186-1195` vs `:1636-1646` |
| SSE `data:` 重组 | `ported/streaming_codex_chat.rs:845-861` vs `ported/codex_chat_history.rs:418-432` |
| provider/model 逻辑 | `cli/src/main.rs:1481-1556` 重实现 `manager/src/lib.rs:105-206,:400-415`；`cli:1346-1388` 重实现 `manager:445-501` |
| `constant_time_equal` | `router/src/lib.rs:1399-1408`（真正恒定时间） vs `web/src/lib.rs:80-83`（`if a.len() != b.len() { return false; }` 提前返回，泄漏长度） |
| 应用目录标识 | 3 套不同写法：`core:567`、`credentials:66`、`credentials:19` |

### 7.6 超大函数（>150 行）

| 函数 | 位置 | 行数 |
|---|---|---|
| `forward_request_v2` | `router/src/lib.rs:597-975` | ~378 |
| `append_responses_item_as_chat_message` | `ported/transform_codex_chat.rs:669-880` | 210 |
| `web_command` | `cli/src/main.rs:1017-1172` | 156 |
| `build_and_install` | `integration/src/lib.rs:155-280` | ~125 |
| `desktop::install` | `desktop/src/lib.rs:351-510` | ~160 |
| `restore` | `integration/src/lib.rs:440-517` | ~78（内联 v1/v2 分叉） |

`forward_request_v2` 把鉴权、4 种 body 解码、模型解析、9 臂协议匹配、header 应用、上游派发与 3 种流式/非流式转换全部塞进一个函数，并返回无类型的 `(Value, &str, bool)` 三元组——这是第 5 节多个协议 bug 难以发现的结构性原因。

---

## 8. 分阶段修复计划

### 阶段 0：止血（先做，不依赖任何产品决策）

| # | 任务 | 验收 |
|---|---|---|
| 0.1 | 新增 `.gitattributes` + `git add --renormalize`（H-05） | `git status` 干净；shell 脚本以 LF 存储；`bash -n` 全部通过 |
| 0.2 | 新增 `.github/workflows/ci.yml`：fmt / clippy / test / `node --check` / `bash -n` / PS parser（C-01） | PR 与 push 上自动运行；故意引入一个格式错误可让 CI 变红 |
| 0.3 | 删除 `untitled-project/`、空 `tests/`、`dist/` 陈旧产物、`.gitignore` 的 `src-tauri` 行（H-04、H-02） | 干净检出后无多余目录 |
| 0.4 | 修复 L-01 Linux service 安装（复制 unit + 原子提交 + 最后写 manifest） | 容器内 `CODEX_MP_INSTALL_SERVICE=1 ./install-linux.sh` 成功；`systemctl --user status` 可见 |
| 0.5 | 修复 S-07 面板 XSS（`planClass` 白名单 + 统一 `escapeAttr`） | 用 `plan_type = 'x"><img src=x onerror=alert(1)>'` 导入账号，断言不产生 DOM 注入 |
| 0.6 | 修复 S-01 令牌泄漏（三处改返回 `AccountSummary`） | 断言测试：`/api/v1/accounts*` 响应体不含 `refresh_token`/`access_token`/`id_token` |
| 0.7 | 修复 L-02 `.deb` 缺 `beforeRemove` | `dpkg -i` + `dpkg -r` 后 `config.toml` 已还原、Router 已停止 |

### 阶段 1：安全（`web` + Electron）

> **前置决策**：S-04 的传输加密方案（TLS / 内网降级 / 外部隧道）必须由产品先定；S-02 与 S-03 与之强相关。

1. **S-02** 密码哈希 → argon2id，含旧格式一次性迁移
2. **S-03** 登录限流 + 指数退避
3. **S-04** 按决策实现传输安全，并在 `--enable` 时打印强警告
4. **S-05** `local_token` 增加 loopback 校验、拒绝空 token、每次启动轮换
5. **S-06** Electron：移除 `webSecurity:false`、加 `will-navigate`/`will-redirect` 守卫、`openExternal` scheme 白名单、加 CSP、`ipcMain` 校验 sender origin
6. **S-08** CORS 改白名单；状态变更接口加 CSRF 防护
7. **B-15** 会话 TTL + 注销接口
8. **B-06** `authorize()` 失败关闭；`RouterState::new()` 收窄可见性

**验收**：新增 `crates/web/tests/security.rs`，覆盖令牌不回传、限流生效、非 loopback 的 `local_token` 被拒、CORS 拒绝非法 origin；Electron 侧补一个导航守卫的单元测试。

### 阶段 2：数据安全与可恢复性

1. **D-01** `config.toml` 改用 `toml_edit` 定点修改（这是本阶段最高价值项——当前每次 `sync` 都在破坏用户配置）
2. **D-02** 两阶段提交（pending manifest → 改状态 → 标记 applied）+ 启动时自动回滚
3. **D-03 / D-04** 调整写出顺序；v2 `restore` 补 `already_restored` 分支
4. **D-05 / D-06** `desktop` 的 restore/install 重排序 + 幂等 + 启动自检
5. **B-11** `restore` 删除路径加包含性校验
6. **B-10** 状态文件统一加跨进程文件锁
7. **L-06** 卸载器在 CLI 缺失时降级为按 manifest 清理

**验收**：为每一个崩溃窗口写一个故障注入测试（在指定步骤后 abort），断言下次启动能恢复；D-01 需要字节级 fixture 测试。

### 阶段 3：进程与生命周期

1. **B-01** Windows 进程匹配对齐 Linux/macOS 语义 + 排除自身
2. **B-02** Router 客户端加 `Policy::none()` + 超时 + `base_url` 解析后 IP 校验
3. **B-03** 启动互斥锁（`create_new` + 陈旧锁回收）
4. **B-04** generation 过期时优雅停机旧实例
5. **B-05** `kill_on_drop(true)` + SIGTERM/控制台事件 + `Drop` 补 `wait()`
6. **B-07** `chrono_iso_now` 改 RFC3339
7. **B-13** `WebState` 保存真实 `IntegrationPaths`
8. **B-12** `history_routes` 加 LRU 上限
9. **B-14** `Option<Json<T>>` 修正

### 阶段 4：协议正确性（`protocol-bridge`）

1. **X-01** 过滤 `null` 透传字段
2. **X-02** 补 `text.format` → `response_format` 映射
3. **X-03** `content_filter` → `incomplete` + `incomplete_details`
4. **X-04 / X-06** SSE 解析失败发 `response.failed` 并 `warn!`；`error: null` 不再致命
5. **X-05 / X-07 / X-08 / X-09 / X-10** 逐项修复并补测
6. **X-11** 确认 `response.model` 覆盖是否有意
7. **P-01** 自定义模型目录改为字段 allowlist，显式声明能力（这同时修复 F5 的能力伪装）
8. **P-02** 删除分叉的 `resolve_route`

**验收**：每个修复配一条回归测试；为 `text.format`、`content_filter`、截断 SSE 帧各建一个 fixture。

### 阶段 5：契约与文档一致性

1. **I-01** 把 `DEVELOPMENT_REMEDIATION_PLAN.md` 归档并重写文件头
2. **I-02** 按决策执行方案 A（删除账号模块）或方案 B（改写承诺 + 令牌入 keyring + 清理备份）
3. **I-03** 统一应用目录标识为单一常量，三平台安装器共用
4. 修正 `README.md` 与代码不符的三处（`auth.json` 承诺、`status` 输出中的 "OAuth: not accessed"、远程访问的安全描述）
5. 修正 `docs/RELEASE.md`（NSIS 而非 Inno Setup）、删除 `setup.iss`（H-01）
6. **H-03** 把 `THIRD_PARTY_NOTICES.md` 加入 `.deb`/`.AppImage`/NSIS 载荷
7. **H-02** 清除 5 处 Tauri 残留

### 阶段 6：收敛臃肿

1. 移除 8 处 `#![allow(clippy::all)]`，改为精确 lint 豁免（7.2）
2. 删除 8 个未使用依赖；`tower` 移到 dev-dependencies（7.4）
3. 合并 7.5 表中的重复实现（`set_private_permissions`、`remove_stale_endpoint`、进程查找、SSE 解析、`constant_time_equal`、provider/model 逻辑）
4. 把 `third_party/cc-switch` 的本地改动抽成可复核的 `.patch`（7.1）
5. 拆分 7.6 表中的超大函数，`forward_request_v2` 优先
6. 抽公共安装器库，消除 7.3 的分叉（这是 L-01/L-08/L-10 的根因）
7. 清理第 6 节全部死代码与死 CSS

**验收**：`cargo machete` 无输出；`grep -rn 'allow(clippy::all)'` 无结果；三平台安装器共享同一个工件清单常量。

### 阶段 7：CI 硬化

1. **C-02** 真实执行 Linux 安装/卸载往返（含 service 与 desktop 开关）
2. **C-06** 把 `stock-omnibridge-e2e.py` 接入 CI（至少在 nightly/manual 通道）
3. **C-03** 发布前校验 portable 归档；默认 `draft: true`
4. **C-04** Actions 固定到 commit SHA
5. **C-05** 加 Rust 与 npm 缓存

---

## 9. 建议新增的自动化门禁

| 门禁 | 目的 | 拦截的问题 |
|---|---|---|
| `cargo clippy -- -D warnings` 且禁止 `#![allow(clippy::all)]` | 恢复 lint 覆盖 | 7.2、X-01/X-06/X-09 |
| `cargo machete` / `cargo udeps` | 阻止未使用依赖 | 7.4 |
| 断言 `/api/v1/accounts*` 响应不含 `refresh_token` | 防止令牌回传 | S-01 |
| 断言 `--local-token ""` 被拒绝、非 loopback 被拒 | 防止免密通道 | S-05 |
| 断言 `config.toml` 注释在 `sync` 后字节级不变 | 防止配置破坏 | D-01 |
| 故障注入：在每个写状态步骤后 abort，断言可恢复 | 防止不可恢复状态 | D-02~D-06 |
| 断言 `resolve_route` 一类死代码不存在（或删除） | 防止逻辑分叉 | P-02 |
| `sha256sum -c` 校验 patch | 防止补丁漂移 | L-07 |
| 安装/卸载往返断言 `config.toml` 已还原 | 保证零污染承诺 | L-02、L-06 |
| 断言 shell 脚本为 LF | 防止 CRLF 污染 | H-05 |

---

## 11. 实施状态（2026-09-17 更新）

> 本节记录本轮修复的落地情况。**未列出的一切修复项仍未完成。**
> 由于开发机没有 Rust 工具链，所有改动**未经编译与测试验证**——`cargo fmt/clippy/test`
> 必须由下一次在具备工具链的环境中执行。

### 11.1 产品决策记录

| 决策项 | 选择 | 影响 |
|---|---|---|
| I-02 `auth.json` 账号功能 | **保留并修复 + 改文档**（方案 B） | 不需要删除 `manager::account`；改为令牌入钥匙串、接口只返回摘要、修正文档承诺 |
| S-04 远程访问传输安全 | **允许明文 HTTP，但 0.0.0.0 绑定强制要求密码** | 不引入 TLS 依赖；CLI/API/绑定三处失败关闭 + 阻塞性安全警告 |

### 11.2 已完成

**Phase 0 止血**

- H-05 新增 `.gitattributes`（shell 脚本强制 LF、PS1/NSH 强制 CRLF、二进制保护）
- C-01 新增 `.github/workflows/ci.yml`：fmt / clippy `-D warnings` / test / `node --check` /
  `bash -n` / PowerShell parser / 补丁校验，并内置 4 条门禁（禁止 `#![allow(clippy::all)]`、
  禁止 shell CRLF、必须存在 CSP、禁止 `webSecurity: false`）
- H-04 删除 `untitled-project/`、空 `tests/`、死文件 `installer/windows/setup.iss`、
  冗余 `apps/electron/package.json`；`.gitignore` 移除已不存在的 `src-tauri` 行
- L-01 Linux `CODEX_MP_INSTALL_SERVICE=1`：补装 systemd unit；`install-router-service.sh`
  支持 `CODEX_MP_UNIT_SOURCE`；安装流程改为"先写 manifest 再执行可选步骤"
- S-07 面板 XSS：`planClass` 改白名单（`sanitizePlanClass`），新增 `escapeAttr`，
  4 处 `data-id` / `data-name` 统一转义
- S-01 令牌泄漏：`capture` / `import` / `switch` 三个接口改返回 `AccountSummary`
- L-02 `.deb` 卸载还原：`package.json` 增加 `fpm: ["--before-remove", ...]`
  （electron-builder **不支持** `beforeRemove`，只能用 fpm 透传）；重写
  `installer/linux/deb/prerm`（精确 `pkill -x`、双分支用户切换、失败可操作提示）

**Phase 1 安全**

- S-02 密码哈希改 **Argon2id**（`Argon2::default()`，19 MiB/t=2/p=1），旧 `salt$sha256`
  格式仍可验证并在下次成功登录时**自动迁移**
- S-03 新增 `LoginRateLimiter`：每 IP 5 次/分钟 + 全局 30 次/分钟滑动窗口，
  超限返回 429 + `Retry-After`
- S-04 三处失败关闭：`api_security_update`、`web remote --enable`、`run_web_server` 绑定前；
  CLI 与启动横幅打印阻塞性明文 HTTP 警告
- S-05 `local_token`：仅接受 loopback 来源、拒绝空 token（空值改为随机生成）
- S-06 Electron 加固：移除 `webSecurity: false`、启用 `sandbox`、新增
  `will-navigate`/`will-redirect` 守卫、`openExternal` 限 http(s)、
  面板加严格 CSP（`index.html` meta + Rust `Content-Security-Policy` 响应头）、
  IPC 增加 `isTrustedSender` 校验、移除 `executeJavaScript` 字符串插值、后端重启加指数退避
- S-08 CORS 改 loopback/`null` 白名单；`constant_time_equal` 去掉长度提前返回
- B-15 会话表改 `HashMap<String, Session>`，绝对有效期 12h + 空闲 60min；
  新增 `POST /api/v1/security/logout`；改密码时吊销其他会话
- B-06 `authorize()` 未配置 token 时返回 503（失败关闭）
- preload 收敛为最小 API（移除未使用且未注册的 `quitApp`/`syncCodex`/`getStatus`），
  新增 `apiBase` 与单向 `onBackendEvent`

**Phase 2 数据安全**

- D-01 `config.toml` 改用 **`toml_edit`** 定点修改：只动 `model_provider`、
  `model_catalog_json`、`[model_providers.omnibridge]`，用户注释/空行/对齐/无关 table
  全部保留（守卫逻辑仍走 `toml::Value`，降低回归面）
- D-04 v2 `restore` 增加 `already_restored` 分支，部分失败不再永久死锁
- L-06/L-08 卸载器：CLI 缺失时降级为按 manifest 清理（不再中止）；
  macOS 补齐完整文件列表并显式删除 launchd plist

**Phase 3 进程与账号安全（I-02 方案 B）**

- 账号令牌移入系统钥匙串（`NativeCredentialStore`，服务名
  `dev.codex-multiprovider.accounts`）；`accounts.json` 通过
  `#[serde(skip_serializing)]` 不再承载任何令牌，旧文件首次加载时自动迁移
  （并有回归测试断言存储文件中不含 `refresh_token`）
- 删除账号时同步删除钥匙串条目
- `auth.bak-switch-*` 备份：原子写 + 0600 + 唯一命名 + 只保留最近 3 份
- B-01 Windows 进程匹配改精确镜像名（`codex.exe`/`chatgpt.exe`/`codex-code-mode-host.exe`）
  + `app-server` 命令行校验 + 排除自身 PID；三平台语义对齐
- `terminate_pid` 返回真实结果：SIGTERM → 轮询 → SIGKILL；`taskkill` 退出码 128 视为已退出
- B-07 `chrono_iso_now()` 输出 RFC 3339（自实现 `civil_from_days`，已用参考实现逐例校验）
- B-02 Router 客户端：`redirect(Policy::none())` + connect 10s + 请求级 600s 超时；
  `normalize_base_url` 拒绝 link-local/云元数据地址（loopback 仍允许，本地大模型需保留）
- B-12 `history_routes` 改 `BoundedRouteMap`（上限 4096，FIFO 淘汰）
- B-13 `WebState` 保存由实际 registry 派生的 `IntegrationPaths`，
  面板不再写到默认目录
- B-14 `api_desktop_install` 改 `Option<Json<T>>`
- 锁中毒不再 panic（改 `unwrap_or_else(|p| p.into_inner())`）

**Phase 4 协议与目录**

- X-01 新增 `non_null()`：显式 JSON `null` 不再透传给严格上游
- X-02 新增 `responses_text_format_to_chat()`：`text.format` → `response_format`
  （`json_schema` / `json_object`，兼容嵌套与扁平两种形状）
- X-03 `finish_reason=content_filter` → `incomplete` + `incomplete_details.reason`；
  新增 `incomplete_reason_from_finish_reason()`，非流式与流式共用
- X-04 畸形 SSE 帧改为发出 `response.failed` + `upstream_sse_malformed`
- X-06 `"error": null` 不再被当作致命错误
- P-01 自定义模型目录：官方专属字段（`shell_type`、`model_messages`、
  `usage_instructions`、`base_instructions`、`supports_web_search` 等）统一置 `null`
  （保留 key 以兼容 schema，而非整模板克隆）；`ModelCapabilities::default().tools`
  由 `true` 改 `false`（能力宣传失败关闭）
- P-02 删除与真实路由逻辑分叉且失败开放的 `resolve_route`，测试改测真实解析函数

**Phase 6 收敛（部分）**

- 8 处 `#![allow(clippy::all)]` 收窄为 `#![allow(clippy::style, clippy::complexity, clippy::perf)]`，
  **correctness / suspicious lint 全部重新启用**（这 8 个文件正是问题最密集处）
- 删除未使用依赖：`router`（anyhow / async-trait / tokio-util / async-stream / http），
  `web`（bytes / tower / tracing / codex-mp-catalog），`catalog`（anyhow），`core`（toml）；
  `router` 的 `tower` 与 `web` 的 `codex-mp-credentials` 移到 `dev-dependencies`
- 新增 `AuthStrategy::credential_header()`，统一 router 与两处模型发现的鉴权头
  （此前 `fetch-models` / `discover-models` 硬编码 Bearer，`api_key`/`header` provider 必然失败）
- 删除死代码：`resolve_route`、`restore_root_string`、`apply_omnibridge_config`
  （`toml::Value` 版）；`crates/cli` 的 `auth_strategy` 不再静默丢弃 `--auth-header`；
  `web access`/`web remote` 无标志时改为非零退出
- `status` 命令不再声称 "OAuth: not accessed by codex-mp"，改为陈述真实策略

**Phase 5 文档**

- `DEVELOPMENT_REMEDIATION_PLAN.md` 移入 `docs/archive/`，文件头重写为
  HISTORICAL ONLY 并逐条列出被推翻的结论
- `README.md`：重写 `auth.json` 承诺（如实说明账号功能）、新增远程访问安全警告块
- `docs/CCSWITCH_MECHANISM_AUDIT_AND_REBUILD_PLAN.md`：修订 §3.1 第 1 条合同
- `docs/RELEASE.md`：修正为 NSIS（非 Inno Setup），并标注 `scripts/build-*.sh`
  与 CI 打包路径的差异

### 11.3 尚未完成

以下条目**本轮未实施**，仍按原文严重度有效：

| 编号 | 内容 | 优先级 |
|---|---|---|
| D-02 | 两阶段提交（pending manifest + 启动自愈）——config 与 manifest 之间的崩溃窗口仍在 | **P1** |
| D-03 | `effective_registry.save()` 失败时 manifest 未移除，安装仍会卡死 | **P1** |
| D-05 | `desktop::restore` 仍先删 runtime 再恢复入口 | **P1** |
| D-06 | `desktop::install` 的 `CODEX_CLI_PATH` 崩溃窗口；macOS plist 未 `launchctl load` | **P1** |
| B-03 | Router 启动无跨进程锁 → 双实例竞争 | **P1** |
| B-04 | generation 过期的 Router 仍被遗弃为孤儿进程 | **P1** |
| B-05 | `RouterSupervisor` 仍无 `kill_on_drop`，仍只处理 Ctrl-C | **P1** |
| B-08 | capability token 仍明文写入 `config.toml` 且不轮换 | P2 |
| B-09 | Windows 端点令牌文件权限仍不校验 | P2 |
| B-10 | 状态文件读-改-写仍无跨进程锁 | P2 |
| B-11 | `integration::restore` 删除路径仍无包含性校验 | P2 |
| X-05 | history hydration 仍会把 `input` 由 object 变形为 array | P2 |
| X-07 | `collapse_system_messages_to_head` 仍不处理 parts 数组形式的 system 消息 | P2 |
| X-08 | 工具输出媒体仍有两条实现路径 | P2 |
| X-09 | `n > 1` 时额外结果仍被静默丢弃 | P2 |
| X-10 | 5 个 pending-reasoning 合并函数与 210 行嵌套 match 未合并 | P3 |
| X-11 | 响应中的 `model` 被覆盖为上游名，是否有意仍待确认 | P3 |
| L-03 | `installer/codex-mp-router.service` 仍硬编码路径，未做占位符替换 | **P1** |
| L-04 | `install-windows.ps1` 仍未检查 service 安装的 `$LASTEXITCODE` | **P1** |
| L-05 | NSIS 卸载器仍未检查 `nsExec` 退出码 | **P1** |
| L-07 | `apply.sh` 仍未自行校验 `SHA256SUMS`（CI 已覆盖） | P2 |
| I-03 | 三套应用目录标识仍未统一 | P2 |
| H-01 | Tauri 残留（`.gitignore` 已清，`router` 的 `tauri` scheme 与测试仍在） | P3 |
| — | 安装器三平台重复代码抽取（Phase 6b） | P2 |
| — | 7.6 超大函数拆分（`forward_request_v2` 等） | P3 |
| — | 7.1 `third_party/cc-switch` 本地改动抽成可复核 patch | P3 |
| — | Phase 7 CI 硬化（release.yml 固定 SHA、缓存、真实安装/卸载 smoke、e2e 接入） | P2 |

### 11.4 下一次在具备工具链的环境中必须先做的事

1. `git add --renormalize .` 并提交一次行尾规范化（`.gitattributes` 已就位，但历史文件
   仍是 CRLF，`ci.yml` 的 LF 门禁需要先跑一次规范化才能通过）。
2. `cargo build --workspace` —— 本轮改动量大且**未经编译**，优先修复编译错误。
3. `cargo clippy --workspace --all-targets -- -D warnings` ——
   8 个文件刚从 `clippy::all` 收窄，correctness/suspicious lint 首次生效，预计会有新告警。
4. `cargo test --workspace` —— 新增测试：web（限流 / 会话过期 / CORS / 令牌不入库 /
   旧哈希迁移）、integration（注释保留 / restore 幂等）、manager（RFC 3339 日期）。
5. `node --check` 三个 JS 文件已本地通过；`apps/panel/main.js` 的
   `sanitizePlanClass` / `bindClick` 需在浏览器中做一次冒烟。

---

## 10. 附录：初始审计方法与局限（针对修复前的代码库）

### 10.1 方法

- 逐 crate、逐文件通读全部 Rust 源码（31 文件 / ~20,400 行）、全部前端 JS（~60 KB）、全部安装器与脚本（24 文件）、CI 工作流、以及 `patches/`。
- 跨仓库 grep 验证每一条"未使用/无调用者"的断言（避免只在本 crate 内搜索导致的误判）。
- 区分 `#[cfg(test)]` 内的 `unwrap()` 与生产代码：生产代码仅 **2 处 `unwrap()` + 16 处 `expect()`**，因此"panic 泛滥"的直觉判断不成立——已按实际数据修正。
- 对每条 P0/P1 结论都回到源码逐行核对（本文档标 ✅ 者）。例如已确认 `protocol-bridge/src/ported/transform.rs:27` 的 `expect("request is an object")` 有前置守卫、实际不可达，因此**未**将其列为可利用缺陷。

### 10.2 明确未验证的部分

- **无法编译**：本机无 Rust 工具链，`cargo check` / `test` / `clippy` 均未运行。所有编译期结论为推断。
- **无法运行**：任何安装器、服务、注册表/`launchctl` 写入、Electron 打包、`.deb` 生命周期。
- **未做的运行时验证**：S-07 的 XSS 未经浏览器实测（结论来自插值点与数据来源的静态追踪）；X 系列协议问题未经真实上游验证。
- **测试套件未执行**：文档声称的 220 个测试的通过状态未在本轮复核。
- 本文档**不改变** `CCSWITCH_MECHANISM_AUDIT_AND_REBUILD_PLAN.md` 中任何 F1–F5 的 `NOT PROVEN` 判定，也不声称任何新增的 PASS。

### 10.3 修复优先级建议（若只能做三件事）

1. **S-07 + S-01 + S-06**：面板 XSS → 令牌泄漏 → Electron 关闭同源策略，这三者串成一条从"导入账号凭据"到"命令执行 + 账号接管"的可利用链路。
2. **D-01 + D-02**：每次 `sync` 都在摧毁用户的 `config.toml` 注释，且存在崩溃后永久不可恢复的窗口——这直接违背产品的"零污染"核心卖点。
3. **L-01 + L-02 + C-01**：Linux 服务安装完全不可用、`.deb` 卸载不还原配置、且没有 CI 能发现这两件事。
