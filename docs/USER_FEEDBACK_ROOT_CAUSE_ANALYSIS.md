# 用户反馈问题根因分析报告

- 日期：2026-09-19
- 性质：**问题研究与根因定位**，不含任何代码修改
- 方法：4 路并行代码探索（router 生命周期 / 凭据存储 / 面板 UI / 登录鉴权）+ 关键结论逐条人工核验（文中所有 `文件:行号` 均已在当前源码 `HEAD = 19564a5` 上复核）
- 基线：本报告基于 2026-09-19 的 `feat(panel): rebuild the web panel...` 提交之后的源码

---

## 0. 问题总览

| # | 现象 | 根因一句话 | 严重度 |
|---|------|-----------|--------|
| 1 | 启动后提示「Router 离线」 | Router 随应用自动拉起、启动失败原因只写 stderr，面板只有四态文案且无任何修复入口；「未安装/需安装」的心智模型与实际架构不符 | 高（体验） |
| 2 | 「加载账号失败：keyring write failed … 2560 chars」 | 账号整包 OAuth token（多枚 JWT 合计必然 >2560 字符）作为单条目写入 Windows 凭据管理器，超出平台硬限制；且「读列表」路径里的 legacy 迁移写入失败会让整个接口报错 | 高（功能不可用） |
| 3 | 网页登录后卡住很久 | 登录成功后 `refreshAll()` 的 `/router/status` 请求排队等后台 Router 启动任务持有的同一把 `Mutex`，启动失败时最坏阻塞 ~35-40 秒；叠加账号接口 3×N 次 keyring 往返 | 高（体验） |
| 4 | 未登录貌似能看到数据 | 业务 API 鉴权本身无洞；观感来自 localStorage 会话 token（服务端 TTL 12h）+ Electron 免密通道。真正的泄露点是未鉴权的 `/security/status` 暴露安全配置 | 中（安全信息） |
| 5 | 侧边栏未与内容栏分离滚动 | 整页只有文档级滚动：html/body 无高度钳制，nav 与 main 都没有 overflow | 中（UI） |
| 6 | 设置页路径里带问号 | Windows `fs::canonicalize` 返回 `\\?\` verbatim 前缀，路径里含字面 `?` 字符，原样透传到面板 | 低 |
| 7 | 网页控制地址缺「在浏览器打开」按钮 | 只渲染纯文本 `<p>`；Electron 侧拦截链路（`window.open` → 系统浏览器）已就绪但无人使用 | 低（功能缺失） |
| 8 | 保存配置无成功提示 | 大多数保存路径其实已有 toast；确认无反馈的是「外观与无障碍偏好」；若用户指其他保存点，需先核对运行版本（面板是编译期/打包期嵌入，旧安装包不会自更新） | 待定位 |

> 症状关联：**问题 1 与问题 3 同源**——Router 后台启动失败/超时（原因只进 stderr）既造成面板「离线」文案，又让登录后的全量刷新卡在 supervisor 锁上。修好启动失败的可见性与锁竞争，两个症状同时消失。

---

## 1. 「Router 离线」提示让人困惑

### 1.1 架构事实：Router 到底是什么

先回答用户提出的分岔判断——「router 是暴力/侵入程序还是友好程序」：

**Router 不是独立安装的程序，而且它实际上已经是 Electron 应用的一部分。**

- Router 是 `codex-mp` 这个单一二进制的 `router` 子命令，并不存在单独的 router 安装物。Electron 的 electron-builder 配置已把 `target/release/codex-mp.exe` 打进 `resources/bin/`（`package.json:46-55`），NSIS 卸载脚本也引用它（`installer/windows/uninstaller.nsh:18-19`）。
- 启动链是三级父子进程：

```
Electron(main.js) --spawn--> codex-mp web start --headless --port=31828
                                 └--tokio::spawn--> RouterSupervisor.start()
                                       └--spawn--> codex-mp --registry <...> router --port 8787
```

- Electron 侧：`apps/electron/main.js:89-152`（`startRustBackend`，带崩溃退避重启）；Rust 侧：`crates/web/src/lib.rs:1956-1975`（web 服务启动时后台拉起 Router）；spawn 参数：`crates/manager/src/lib.rs:976-992`。
- 「侵入性」评估（用户关心 Clash Verge TUN 驱动式的装卸模型是否适用）：

| 维度 | 结论 | 证据 |
|---|---|---|
| 监听范围 | 强制 loopback `127.0.0.1`，非 loopback 直接拒绝 | `crates/router/src/lib.rs:44,79-84` |
| 常驻性 | Electron 模式下**不常驻**：`kill_on_drop(true)`，宿主退出即死 | `crates/manager/src/lib.rs:990` |
| 对 codex 的改动 | 只有 `codex-mp sync` 改写 `~/.codex/config.toml`（有 manifest 两阶段记录，可完整回滚）；router 进程本身不动任何文件 | `crates/integration/src/lib.rs:122-125,493-524` |
| 系统位置 | 仅在用户**显式**安装可选服务时才写，且全为用户级（systemd user unit / LaunchAgent / 用户级计划任务），不写注册表、不需要管理员 | `installer/install-router-service*.sh|.ps1` |
| 驱动/内核 | 无 | — |

**结论：router 是「友好程序」，用户的第二种直觉（做成 Electron 应用的一部分、不分体）就是当前现状。Clash Verge TUN 驱动式的「未安装→一键安装/卸载」模型在这里既不成立也无必要。**

### 1.2 「离线」的确切来源

- 全仓库唯一的「离线」文案在 `apps/panel/main.js:876-906`（`refreshRouterStatus`）：

```js
const status = await api("/api/v1/router/status");
...
} else {
  badge.className = "m3-chip m3-chip--error";
  text.textContent = "Router 未运行";
  metricVal.textContent = "离线";            // main.js:897
  metricDesc.textContent = "后台服务未启动";
}
```

- 判定链路：panel → `GET /api/v1/router/status`（路由 `crates/web/src/lib.rs:392`）→ handler `api_router_status`（`lib.rs:974-983`）→ `RouterSupervisor::status()`（`crates/manager/src/lib.rs:1110-1156`）。`running=false` 时面板显示「离线」。
- 该状态只在页面加载和手动刷新时拉取一次（`main.js:1982`、`1626-1630`），**无轮询**。
- 「离线」的真实语义：web 后端（31828）活着，但它启动/复用 router 子进程**失败**（若 31828 都没起来，前端 catch 分支显示的是「Router 状态未知」，`main.js:900-905`）。

### 1.3 根因：失败原因对用户完全不可见，且无任何修复入口

1. **启动失败原因只打到 Rust 进程 stderr**（`crates/web/src/lib.rs:1968-1972`）：

```rust
tokio::spawn(async move {
    match supervisor_clone.lock().await.start().await {
        Ok(endpoint) => { println!(" OmniBridge Router 已就绪: {}", endpoint.base_url); }
        Err(error) => {
            eprintln!("codex-mp: the web panel started but the Router did not ({error}); ...");
        }
    }
});
```

   Electron 打包版里 stderr 只进主进程 console（`apps/electron/main.js:122-124`），最终用户无法看到。`RouterSupervisor::start()` 失败的典型诱因：8787 端口被占用（端口被强制要求与 `config.toml` 公告的 `base_url` 一致，`ManagerError::PortInUse`）、子进程 15 秒内未就绪（`STARTUP_TIMEOUT`，`crates/manager/src/lib.rs:28`）、跨进程启动锁等待 20 秒（`STARTUP_LOCK_TIMEOUT`，`lib.rs:32`）、二进制路径解析失败、registry 代次不匹配（`StaleGeneration`）。

2. **面板没有操作入口**：web API 的 router 相关端点只有 `GET /router/status`（`crates/web/src/lib.rs:392`），没有 start/stop/restart。面板上的 router 元素全是只读展示（顶栏徽章 `apps/panel/index.html:47-50`、总览卡 `index.html:149-159`）。Electron 的 `reportToRenderer`（`main.js:79-86`）只转发 **web 后端**进程的启动失败，router 子进程的失败不会推送。

3. **心智模型错配**：用户看到「离线/后台服务未启动」，自然理解为「有个服务没装/没启动」，于是寻找「安装」入口——但架构上不存在这个动作，实际需要的是「告诉我为什么失败 + 让我重试」。状态 API 也不返回失败原因字段（`RouterStatus` 只有 `running/healthy` 两个布尔，`crates/manager/src/lib.rs:1110-1156`）。

### 1.4 修复方向（建议，不实施）

- 后端：把 `supervisor.start()` 的错误存入状态（`RouterStatus` 增加 `last_error` 或新增 `GET /router/last-error`），并新增 `POST /router/restart`；状态探测与启动互斥解耦（见问题 3）。
- 面板：四态文案改为可操作的失败态——「Router 启动失败：端口 8787 被占用」+「重试」按钮；顶栏徽章可点击跳到带操作的卡片。
- 可选：若希望进一步贴近「整机一个进程」的直觉，可评估让 Electron 主进程直接管理 router 生命周期（少一层 `codex-mp web` 中转），但这属于架构级改动，优先级低于「可见性 + 重试」这两个小改动。

---

## 2. 账号凭据存储报错（Windows keyring 2560 字符限制）

### 2.1 现象

面板报「加载账号失败」：`Credential store error: credential backend error: keyring write failed; choose --secret-backend file explicitly to enable the 0600 file backend: Value of 'password encoded as UTF-16' is longer than the platform limit of 2560 chars`

### 2.2 根因

**把单个账号的完整 OAuth token 集整体序列化成一条 JSON、作为单条凭据写入系统钥匙串，与 Windows 凭据管理器的单条 2560 字符（UTF-16 计）平台硬限制不兼容。**

- 存储结构（`crates/manager/src/account.rs:28-30,138-148`）：service = `dev.codex-multiprovider.accounts`，reference/user = `account:<uuid>`，值 = `serde_json::to_string(&AccountTokens)`，其中 `AccountTokens = { id_token, access_token, refresh_token, account_id }`。ChatGPT 的 id_token / access_token 都是 JWT（各约 1000-2500+ 字符），三枚打包成 JSON 后总长 3000-6000+ 字符，**在 Windows 上必然超限**；keyring crate 在 `CredWrite` 前预校验（CRED_MAX_CREDENTIAL_BLOB）直接拒绝，即报错末段「Value of 'password encoded as UTF-16' is longer than the platform limit of 2560 chars」（该句来自 keyring 依赖，非本项目代码）。
- 两个写入点：
  - 常规保存：`account.rs:425-443`（`save_file`，`self.credentials.set(&reference, &secret)?`，行 442）——收纳/导入/切换账号必经，**在 Windows 上全部必失败**。
  - legacy 迁移（本次报错的触发点）：`account.rs:382-391`（`load_file`）——旧版本写出的 `accounts.json` 内嵌 tokens，每次读取时尝试迁入 keyring 并重写文件；该 `self.credentials.set(...)?` 的 `?` 让**整个 `load_file` 失败** → `list_accounts` 失败 → `GET /api/v1/accounts` 500 → 面板「加载账号失败」（前端文案 `apps/panel/main.js:1164`，后端 handler `crates/web/src/lib.rs:413,1615-1628`）。
- 错误链三层拼接：`AccountError::Credential`（`account.rs:133-134`，前缀 "Credential store error: "）→ `CredentialStoreError::Backend`（`crates/credentials/src/lib.rs:139-141`，前缀 "credential backend error: "）→ 自定义包装（`credentials/src/lib.rs:288-307`，"keyring write failed; choose --secret-backend file …"）→ keyring crate 原始信息。
- 逃生门：`--secret-backend file` / 环境变量 `CODEX_MP_SECRET_BACKEND=file`（`crates/cli/src/main.rs:41-42,396-400`；检测 `credentials/src/lib.rs:176-178`）会改走 0600 文件后端（`<config_dir>/.credentials`）。Electron spawn 后端时未注入该变量（`apps/electron/main.js:95-106` 的 env 只有 `CODEX_MP_LOCAL_TOKEN`），所以桌面用户踩不到逃生门。

### 2.3 与 Router 的关系

**无关。** Router 对凭据只读不写：`crates/router/src/lib.rs:277-297` 经 `codex_mp_credentials::get_blocking` 读取 **provider** 凭据（另一个 service `dev.codex-multiprovider`，单条 API key 不会超限），从不接触账号 service `dev.codex-multiprovider.accounts`，启动时也没有任何凭据同步动作。

### 2.4 影响

- Windows 上账号收纳/导入/切换功能实际不可用（保存 token 必超限）。
- 只要本地存在「旧格式 accounts.json（内嵌 tokens）」，账号列表接口持续 500，面板永远报「加载账号失败」。

### 2.5 修复方向（建议，不实施）

- 短期缓解：Electron 启动后端时注入 `CODEX_MP_SECRET_BACKEND=file`；或 CLI 文档引导。
- 长期：
  - 按字段分片存储（`account:<id>:id_token` / `:access_token` / `:refresh_token` 多条目），写入前做长度预检；
  - 超限自动降级 file backend，或 Windows 平台对**大 secret** 默认 file backend（保留 keyring 存小 key）；
  - `load_file` 的迁移写入改为容错——迁移失败不应让整个列表接口 500（可保留内嵌 tokens 返回并在响应中带 warning）。

---

## 3. 网页登录后卡住很久

### 3.1 根因链（三层叠加，主次分明）

**主因：`/router/status` 与后台 Router 启动任务共用同一把 `Mutex`，启动慢/失败时状态查询被长时间阻塞。**

- 登录成功后前端立即全量刷新：`apps/panel/main.js:1579`（`await refreshAll()`），且整个流程包在 `withBusy` 里（`main.js:1562`），期间提交按钮 spinner 不消失、面板骨架屏不动。
- `refreshAll()`（`main.js:1523-1534`）并行请求 5 组数据，其中 `/api/v1/router/status` 的 handler 要拿 supervisor 锁（`crates/web/src/lib.rs:975`）：

```rust
async fn api_router_status(State(state): State<WebState>) -> Response {
    match state.supervisor.lock().await.status().await { ... }
```

- 而后台 Router 启动任务**在整个 `start()` 期间持有同一把锁**（`crates/web/src/lib.rs:1962-1964`）：

```rust
tokio::spawn(async move {
    match supervisor_clone.lock().await.start().await { ... }
```

- `RouterSupervisor::start()` 最坏耗时（常量见 `crates/manager/src/lib.rs:28-37`）：跨进程启动锁等待 20s（`STARTUP_LOCK_TIMEOUT`）+ 健康探测每次 3s（`PROBE_TIMEOUT`）+ 子进程就绪超时 15s（`STARTUP_TIMEOUT`）≈ **35-40 秒**。Router 启动失败（端口占用、杀软拦截、慢盘）时必然吃满超时——这正好与用户同时看到的「router 离线」吻合（两个症状同源，见 §0 关联说明）。前端 fetch 无超时设置。

**次因：账号接口一轮刷新做 3×N 次 keyring 往返。**

- `/accounts` → `list_accounts()`（`account.rs:533`）→ `load_file()`（每个账号一次 keyring 读，`account.rs:394`）；同一响应路径还会再调 `check_active_status()`（`account.rs:535,469`）→ 又一次 `load_file()`；前端 `refreshAccounts` 同时请求 `/accounts` 与 `/accounts/active`（`main.js:1110-1113`）。Windows 凭据管理器单次调用不快，账号多时显著放大。

**叠加因素：**

- 密码校验 Argon2id（19 MiB / t=2 / p=1，`crates/web/src/lib.rs:98-102`）直接内联在 async handler 里执行（`lib.rs:794` 一带），单次约 0.1-0.5s，弱 CPU 更久；若存量 hash 是旧版格式，首次正确登录还会再跑一次完整 argon2 做迁移。登录路径**没有任何故意 sleep/延迟**（已全文件排查）。
- 限流语义：60s 窗口内每 IP 5 次 / 全局 30 次失败（`lib.rs:77-79`），超限后**即使密码正确也返回 429**，需等最旧失败滑出 60s 窗口（最长约 60s）；且前端优先显示英文 `data.error`（"TooManyAttempts"）而不是中文 `data.message`（`main.js:1572`：`throw new Error(data.error || "登录失败，密码错误")`，`api()` 的通用错误处理同样优先 `error` 字段，`main.js:93`）。
- 登录 handler 内的 registry 文件锁若被其他进程（如 CLI）占用，最长等待 120s（`crates/core/src/lib.rs:717` `FILE_LOCK_STALE`）。

### 3.2 修复方向（建议，不实施）

- 状态读与启动解耦：`/router/status` 不等锁（`try_lock` + 缓存快照，或 `start()` 内部缩短持锁区间——只在 spawn 前后短暂加锁，等待子进程就绪阶段用状态机表达「启动中」）。
- `RouterStatus` 携带 `starting` 与 `last_error` 字段，前端把「启动中」与「失败原因」区分开（当前 `running=true, healthy=false` 会被显示为「启动中…」，与真实失败混淆，`main.js:889-893`）。
- 错误文案优先取 `data.message`；登录按钮的 busy 状态在 `hideLoginDialog()` 后即可解除，不必等同 `refreshAll()`。
- 账号接口减少重复 `load_file()`（列表与活跃状态共享一次读取）。

---

## 4. 未登录（貌似）就能看到数据

### 4.1 鉴权现状核验（结论：业务数据接口没有裸奔）

路由表（`crates/web/src/lib.rs:389-444`）：24 个业务端点（providers/models/accounts/desktop/catalog/security-update）**全部**挂 `auth_middleware`；未鉴权的只有：

| 端点 | 位置 | 说明 |
|---|---|---|
| `GET /api/v1/security/status` | `lib.rs:429` | 返回 `web_enabled / password_set / allow_remote / bind_addr / port`（侦察价值，见 4.3） |
| `POST /api/v1/security/login` | `lib.rs:430` | 设计如此，有限流 |
| `POST /api/v1/security/logout` | `lib.rs:433` | 仅删除出示的 token，无危害 |
| 全部静态资源（SPA 外壳） | `lib.rs:593-692` | rust-embed 只内嵌 `apps/panel`，无任意文件读取 |

`auth_middleware`（`lib.rs:501-589`）四步 fail-closed：loopback+local_token 直通 → 未开 web 访问 403 → 开了 web 访问但没设密码 403 → session 校验（绑定当前密码 hash，改密即全部失效）。

### 4.2 「未登录就能看到数据」的真实解释

1. **localStorage 持久会话（最可能）**：登录成功后 token 存 `localStorage`（`main.js:1575`），服务端 session TTL 12 小时、空闲 1 小时（`lib.rs:71-72`）。登录过一次后，重开浏览器/新标签页不再弹登录框，直接带 token 拉到全部数据（启动即读 localStorage，`main.js:32-34,60-68`；登录框只在收到 401 时弹出，`main.js:76-89`）——用户以为「没登录」，实际 token 仍有效。
2. **Electron 桌面端免密通道**：loopback + `local_token` 直接放行（`lib.rs:535-541`），桌面端用户永远免密码看到数据（产品设计如此，README 亦有说明）。
3. 视觉因素：SPA 外壳与骨架屏先渲染，登录框是后盖上的 modal，网络慢时有一瞬间「能看到页面结构」。

### 4.3 真正的问题：`GET /api/v1/security/status` 未鉴权

任何能连到端口的主机（远程访问开启时即局域网任意设备）无需任何凭据即可得知：该机是否运行本面板、是否设置了密码、是否开放远程、绑定地址与端口。这是侦察面信息（尤其「未设密码」的实例可被快速识别），建议最小化：未登录时只返回 `password_set` 与 `web_enabled`，或整体纳入鉴权。

### 4.4 修复方向（建议，不实施）

- 会话 token 改 `sessionStorage`（关浏览器即失效），或提供「记住此设备」可选项；
- 未登录首屏主动前置登录框（当前是等第一个 401 才弹）；
- `security/status` 收敛字段或加鉴权。

---

## 5. 侧边栏未与内容栏分离滚动

### 5.1 根因（布局 CSS：整页只有一个滚动容器——文档本身）

- `apps/panel/base.css:17-41`：html/body 无高度钳制（`min-height: 100dvh` 会随内容撑高），只有 `overflow-x: hidden`，纵向放任文档滚动：

```css
body {
  min-height: 100dvh;
  display: flex;
  flex-direction: column;
  overflow-x: hidden;   /* 纵向未裁剪 */
}
```

- `apps/panel/layout.css:12-16`：`.m3-layout-root` 同样是 `min-height: 100dvh`（非 `height`）。
- `layout.css:206-210`：`.m3-app-body { display:flex; flex:1; min-block-size:0 }`——`min-block-size:0` 本意是让子元素可收缩滚动，但**父级没有固定高度时不起作用**。
- `layout.css:218-225`（`.m3-navigation` 基础样式）与 `layout.css:311-316`（≥1200px 抽屉形态）：只设宽度/边框，**无 `overflow-y`、无高度约束**。
- `layout.css:434-442`（`.m3-main-content`）：同样没有 `overflow-y` 与高度上限。
- 顶栏是 `position: sticky`（`layout.css:30-32`），所以滚动时顶栏「粘住」、nav 抽屉随文档滚走——观感即「侧边栏和内容一起滚」。
- 全面板唯一真正的内滚容器是模型发现列表 `.m3-discover__items { max-block-size:260px; overflow-y:auto }`（`layout.css:915-921`）。

### 5.2 修复方向（建议，不实施）

- `html, body`（或 `.m3-layout-root`）改为 `height: 100dvh` + `overflow: hidden`；
- `.m3-app-body` 保持 `min-block-size: 0`（父级固定高度后即生效）；
- `.m3-navigation`（三种形态中的 rail/drawer）与 `.m3-main-content` 分别 `overflow-y: auto`；
- 注意验证三种导航形态（窄屏底部导航栏 / 中屏导航轨道 / 宽屏抽屉）与「减弱动效」偏好下的滚动表现；`scroll-behavior: smooth`（base.css:19）在内滚容器化后应迁移到具体容器。

---

## 6. 设置页路径里带问号

### 6.1 根因：不是乱码，是 Windows verbatim 路径前缀里的字面 `?`

- 后端：`crates/desktop/src/lib.rs:142`——`DesktopPaths::from_entrypoint` 用 `fs::canonicalize()` 解析入口，**Windows 上 `std::fs::canonicalize` 返回 verbatim 形式**，即 `\\?\C:\Users\...\codex.exe`（UNC 则为 `\\?\UNC\192.168.5.23\...`）。`DesktopStatus.entrypoint`（`desktop/src/lib.rs:287-299`）经 serde 原样序列化进 JSON。
- 前端：`apps/panel/main.js:925` 纯插值显示 `status.entrypoint`，无任何转换。
- 排除项：不是字体缺字（缺字形显示回退字体或豆腐块，不会产生字面 `?`）；不是 GBK/UTF-8 转换（web 层只有 `.display().to_string()` 与 serde 序列化，无编码转换代码）；非 UTF-8 路径的 serde 行为是 `to_string_lossy` → U+FFFD「�」，与本案主因不同。

### 6.2 修复方向（建议，不实施）

- 后端返回前 strip `\\?\` 前缀（可改用 `dunce::canonicalize`，或手写 strip：`\\?\UNC\` → `\\`、`\\?\` → ``）；
- 顺带审计仓库内其他 `fs::canonicalize` 的对外展示路径（总览页同步成功 toast 也直接显示后端 `catalog_path`，`main.js:1620`）。

---

## 7. 网页控制地址缺「在浏览器打开」按钮

### 7.1 现状

- 地址展示：`apps/panel/main.js:939-977`（`refreshSecurityStatus`）把 URL 写进 `#settings-security-tip`（`index.html:449`）——**纯 `<p>` 文本**，无链接、无按钮、无复制。
- Electron 能力现状：主进程已有安全封装 `openExternalSafely`（`apps/electron/main.js:154-169`，scheme 白名单 http/https）并挂在 `setWindowOpenHandler` 上（`main.js:279-282`）——渲染层 `window.open(http…)` 会被拒绝开新窗并转交系统浏览器；**但面板没有任何地方调用它**。preload 暴露的 IPC 里也没有显式 `openExternal`（`apps/electron/preload.js:9-20`：只有 isElectron/localToken/apiBase/窗口控制/onBackendEvent）。
- CSP 只约束 fetch（`connect-src`），不阻止导航，所以现有通道可直接复用。

### 7.2 修复方向（建议，不实施）

- 在 `#settings-security-tip` 旁加「在浏览器中打开」按钮：Electron 环境走 `window.open(url)`（现有拦截链转系统浏览器），纯浏览器环境 `window.open` 即新标签页；或新增显式 `open-external` IPC（preload + `isTrustedSender` 校验）。
- 顺带可加「复制地址」按钮（面板已有类似交互模式可参考）。

---

## 8. 保存配置无成功提示

### 8.1 核验结果（与直觉相反：当前源码大多数保存路径有 toast）

| 保存路径 | 提示 | 位置 |
|---|---|---|
| 访问安全配置 | `notify("访问安全配置已更新")` | `apps/panel/main.js:1911` |
| Provider 新增/编辑 | `notify("Provider 已保存至本地安全存储…")` / `notify("Provider 已更新")` | `main.js:1757` / `1746` |
| 模型编辑/启停/删除 | 各有 notify | `main.js:1253,1219,1271` |
| 账号各类操作 | 各有 notify | `main.js:1039-1096` |
| 同步到 Codex | `notify("Codex Catalog 同步成功…")` | `main.js:1620` |
| **外观与无障碍偏好（主题/对比度/动效）** | **无任何反馈**（点击即写 localStorage） | `main.js:596-604` |

toast 组件本身健全（`#m3-snackbar`，`index.html:706-711`；队列实现 `main.js:110-168`；样式 `components.css:1477-1543`，`position:fixed` + `z-index:9999`，4 秒自动消失）。

历史核查：2026-09-16 引入设置页的版本（`2e050a6`）与 09-19 面板重写后的版本，安全配置保存**均有** notify（旧版 `notify("访问安全配置已更新！")`，`2e050a6:apps/panel/main.js:1219`）。

### 8.2 可能的解释（按可能性排序）

1. 用户指的是「外观与无障碍偏好」这类静默保存（确认无反馈）；
2. 用户运行的是**旧构建**：面板有两份分发路径——Rust 二进制用 `RustEmbed` 在**编译期**嵌入 `apps/panel`（`crates/web/src/lib.rs:37-39`），Electron 打包用 `files: apps/panel/**/*`（`package.json:28-34`）并优先 `loadFile` 本地副本（`apps/electron/main.js:238-246`）。旧安装包里的面板不会自动更新，toast 与文案可能与当前源码不一致；
3. toast 4 秒即逝，用户操作后视线在别处未注意到（尤其是提交按钮本身无 busy 态，安全表单保存期间无任何按钮内反馈）。

### 8.3 修复方向（建议，不实施）

- 外观偏好加轻量确认（或视觉即时反馈已足够，可加 `aria-live` 提示）；
- 提交按钮加 busy 态（安全表单目前未包 `withBusy`）；
- 设置页显示面板/后端版本号（便于排查「旧构建」类问题）；
- 与用户确认具体是哪个保存动作无提示，以便精确定位（若其运行版本早于 09-16，则该版本设置页形态不同，需单独核对）。

---

## 附录 A：本次核验过的关键代码位置

| 文件 | 位置 | 内容 |
|---|---|---|
| `apps/panel/main.js` | 32-34, 39-104 | api() 鉴权头、401 处理、resolveApiUrl |
| `apps/panel/main.js` | 110-168 | snackbar 队列 |
| `apps/panel/main.js` | 596-604 | 外观偏好（无反馈保存） |
| `apps/panel/main.js` | 876-906 | refreshRouterStatus（「离线」文案） |
| `apps/panel/main.js` | 908-937 | refreshDesktopStatus（entrypoint 显示） |
| `apps/panel/main.js` | 939-977 | refreshSecurityStatus（控制地址纯文本） |
| `apps/panel/main.js` | 1110-1113, 1164 | 账号请求与「加载账号失败」 |
| `apps/panel/main.js` | 1523-1534, 1579 | refreshAll 与登录后刷新 |
| `apps/panel/main.js` | 1555-1586, 1588-1610 | 登录/登出流程（localStorage token） |
| `apps/panel/main.js` | 1890-1916 | 安全表单保存（有 notify） |
| `apps/panel/base.css` | 17-41 | html/body 无高度钳制 |
| `apps/panel/layout.css` | 206-225, 311-316, 434-442 | 布局无滚动容器 |
| `apps/panel/components.css` | 1477-1543 | snackbar 样式 |
| `apps/electron/main.js` | 31, 35-77, 89-152 | PANEL_DIR、二进制定位、spawn 后端 |
| `apps/electron/main.js` | 154-169, 279-282 | openExternalSafely 与拦截链 |
| `apps/electron/main.js` | 238-246 | 面板加载优先本地文件 |
| `apps/electron/preload.js` | 9-20 | IPC 白名单（无 openExternal） |
| `crates/web/src/lib.rs` | 28-37 常量区 | SESSION_TTL 12h / 限流窗口 / Argon2id 参数 |
| `crates/web/src/lib.rs` | 389-444 | 路由表与鉴权分层 |
| `crates/web/src/lib.rs` | 501-589 | auth_middleware（fail-closed） |
| `crates/web/src/lib.rs` | 974-983 | api_router_status（等 supervisor 锁） |
| `crates/web/src/lib.rs` | 1956-1975 | 后台拉起 Router（持锁 start，失败只进 stderr） |
| `crates/manager/src/lib.rs` | 28-37 | STARTUP/PROBE/LOCK 超时常量 |
| `crates/manager/src/lib.rs` | 976-992, 1110-1156 | router spawn 与 status() |
| `crates/manager/src/account.rs` | 28-30, 133-148 | 账号 keyring service 与 AccountTokens |
| `crates/manager/src/account.rs` | 364-417 | load_file（legacy 迁移写入，失败即 500） |
| `crates/manager/src/account.rs` | 425-454 | save_file（整包 token 写 keyring） |
| `crates/credentials/src/lib.rs` | 139-141, 176-178, 288-307 | 错误包装与 file backend 开关 |
| `crates/desktop/src/lib.rs` | 136-162, 287-299 | canonicalize（\\?\ 前缀来源） |
| `crates/router/src/lib.rs` | 44, 79-84, 277-297 | loopback 强制、凭据只读 |
| `package.json` | 28-55 | 面板/二进制打包路径 |

## 附录 B：用户疑问的直接回答

1. **router 要不要做成「未安装→一键安装/卸载」？** 不需要也不成立：router 不是独立安装物，它就是随 Electron 分发的 `codex-mp` 的子命令，且强制 loopback、不常驻、不写注册表/驱动，唯一的配置改动（config.toml）有 manifest 可回滚。缺的不是装卸模型，而是**失败可见性**（错误目前只进 stderr）和**重试入口**（web API 连 restart 都没有）。
2. **keyring 报错与 router 有关吗？** 无关。router 只读 provider 凭据；报错在账号管理侧（token 整包超 Windows 2560 字符限制），且由「读列表时的 legacy 迁移写入失败」触发。
3. **未登录看到数据是漏洞吗？** 业务 API 鉴权无洞；是 12 小时 localStorage 会话 + 桌面免密通道造成的观感。真正需要收紧的是未鉴权的 `/security/status` 信息暴露。
