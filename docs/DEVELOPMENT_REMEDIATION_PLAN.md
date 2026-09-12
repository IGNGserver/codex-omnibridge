# Codex MultiProvider 后续整改与实施路线

> 文档状态：审计后续实施基线
>
> 审计日期：2026-09-09
>
> 当前结论：`FAIL`（P0 patched Core per-turn 路由代码已落地，但真实 CLI/app-server、Desktop、Remote 和 Provider E2E 仍未证明）
>
> 本文档不是功能愿望清单，而是后续实现必须遵守的架构约束、实施顺序、验收门槛和交接说明。

---

## 1. 产品目标

本项目最终要实现的不是一个独立的 Provider Switcher，也不是一个重新制作的模型选择器，而是：

```text
Codex Desktop / CLI / Remote app-server
        │
        │ Codex 自己仍然认为 provider = openai
        ▼
Codex 原生 Model Catalog / Model Picker
        │
        ├── 官方模型
        │       → Codex 原生 OpenAI / ChatGPT backend
        │
        └── namespaced Custom model
                → 本地 Router
                → Provider Registry
                → 第三方 Provider
```

用户应该能够在 Codex 原生模型菜单中使用：

```text
GPT-5.6 Luna
NewAPI / GPT-5.6 Terra
NewAPI / Qwen
OpenRouter / DeepSeek
Local / Qwen
```

并在同一个 Thread 中完成：

```text
Official → Custom → Official
```

无需：

- 修改或切换 Codex `model_provider`；
- 创建 `multiprovider`、`newapi`、`openrouter`、`local_proxy` 等 Codex Provider；
- 修改 ChatGPT OAuth 或 `auth.json`；
- 退出 ChatGPT Account；
- 新建 Thread；
- 重启 Codex；
- 重新登录；
- 将 API Key 写入普通配置文件；
- 把日常模型选择放到本项目 GUI 中。

---

## 2. 当前审计结论

### 2.1 已确认成立

| 能力 | 状态 | 证据 |
|---|---|---|
| 不修改 `model_provider` | PASS | `crates/integration/src/lib.rs` 只管理 root-level `model_catalog_json` |
| 不创建新的 Codex Provider | PASS | 当前仓库没有 Provider Switcher / `multiprovider` 配置注入 |
| 不读取或修改 `auth.json` | PASS | 集成层未访问 ChatGPT OAuth 文件 |
| API Key 与普通配置隔离 | PASS | `crates/credentials/src/lib.rs` 使用 keyring，Registry 只保存引用 |
| 官方 Catalog 由当前安装版 Codex 发现 | PASS | `codex debug models --bundled` |
| 官方 + Custom model 可进入原生 Catalog | PASS | 临时 `CODEX_HOME` 下 `codex debug models` 与 app-server `model/list` 可见 |
| namespaced Custom ID | PASS | `<provider-id>/<upstream-model-id>` |
| 独立 Router 的基础 Custom 路由 | PASS | Router 单元测试及 mock upstream 测试通过 |
| restore 的用户修改保护 | PASS | manifest 校验和 `UserChangedManagedField` |

### 2.2 当前阻塞

| 阻塞 | 状态 | 结论 |
|---|---|---|
| Codex Core/app-server → Router 的 per-turn 数据路径 | P0 实现 / NOT PROVEN | patched Core 已在真实 `ModelClientSession::stream()` 分流；尚未通过真实 Codex caller + Router + Provider turn E2E |
| 同一 Thread Official → Custom → Official | NOT PROVEN | patched Core 路由分支已存在，但尚无真实 Thread 三轮证据 |
| Chat Completions → Responses 响应归一化 | P1 未完成 | 当前只转换请求，响应原样透传 |
| 跨模型 Context Boundary | P1 未完成 | 当前转换会丢弃或降级多个 Codex-specific item |
| Router 客户端认证 | PASS（loopback token V1） | 随机 capability token + 0600 endpoint file；仍不是 Unix socket / named pipe |
| 真实 Desktop / Bridge | 未实现或未验收 | 无真实 Desktop caller 证据 |
| Remote Desktop → Remote Host | 未验收 | 无远程 app-server / Router E2E 证据 |
| Upgrade / repair / fail-safe | P2 未完成 | `repair` 只是再次执行安装流程 |
| Windows `Setup.exe` 与完整 Linux 生命周期 | P2 未完成 | Linux installer 当前只复制 CLI binary |

### 2.3 当前仓库的准确定位

当前仓库应被描述为：

```text
Codex MultiProvider Linux CLI / Catalog / Router 技术基础 V1
```

不能描述为：

```text
完整 Codex Desktop MultiProvider
完整同 Thread 跨 Provider 切换
已支持真实第三方推理
```

---

## 3. 不可破坏的架构不变量

后续任何实现、补丁、Installer、Bridge 或测试都必须满足以下不变量。

### I1：Codex Provider 永远保持 `openai`

必须保持：

```toml
model_provider = "openai"
```

不得通过以下方式规避路由问题：

```toml
model_provider = "multiprovider"
model_provider = "newapi"
model_provider = "openrouter"
model_provider = "local_proxy"
```

也不得创建新的 `[model_providers.*]` 作为第三方模型切换机制。

### I2：ChatGPT 身份始终保持有效

不得修改或接管：

```text
auth.json
ChatGPT OAuth token
ChatGPT Account state
chatgpt_base_url
```

Custom Provider 的 API Key 只能属于本项目 Router，不能变成 Codex 的登录凭据。

### I3：Thread Provider 不迁移

安装前创建的官方 Thread 必须仍然能够直接 resume：

```text
Thread provider = openai
```

Custom model 的选择必须是 turn-level logical routing，而不是 Thread migration。

### I4：官方模型不进入第三方兼容层

官方模型请求必须保持 Codex 原生路径，不能为了适配第三方而删除：

- Authorization；
- reasoning；
- hosted tools；
- Web Search；
- Image Generation；
- compaction；
- Responses items；
- Fast mode；
- 未来新增的官方能力。

### I5：Custom model 才进入 Router / Adapter

只有 namespaced logical model 才能进入：

```text
Local Router
Provider Registry
Credential Store
Context Compatibility Layer
```

第三方请求必须移除官方 Authorization，并替换为对应 Provider 的 credential。

### I6：API Key 不进入普通文件和日志

不得写入：

```text
providers.json
models.json
integration.json
config.toml
stdout
普通 tracing 日志
crash report
```

### I7：不依赖重启完成日常切换

模型切换应是：

```text
当前 Thread
    ↓
选择另一个 model
    ↓
下一轮 turn 生效
```

不能要求修改 config 后重启 Codex。

---

## 4. 后续实施阶段

## Phase 0：建立实现前基线

### 目标

在改动 Core/app-server 前，锁定当前 Codex 版本、协议和调用路径。

### 必须完成

1. 记录当前 Codex binary 路径和版本；
2. 记录上游 `openai/codex` commit SHA；
3. 重新检查：
   - `model_catalog_json` 加载位置；
   - app-server `model/list`；
   - `thread/start`；
   - `turn/start`；
   - Thread provider 初始化；
   - ModelClient 生命周期；
4. 确认当前 Codex Desktop 使用的 Bridge/app-server caller；
5. 建立一个隔离的临时 `CODEX_HOME`；
6. 禁止在未确认调用路径前进行大范围修改。

### 输出

必须形成一份简短的实现决策记录，明确选择：

```text
受控 patched Core
```

还是：

```text
app-server/Core 内置 logical-model dispatcher
```

如果只能通过创建新的 Codex Provider 实现，必须停止并报告架构不可接受，不得继续绕过。

---

## Phase 1：实现真正的 per-turn logical routing（P0）

### 目标

让 Codex 原生 turn 根据 logical model 选择 backend，同时保持 Thread/provider/auth 不变。

### 目标行为

```text
turn model = gpt-5.6-luna
        → 原生 Codex OpenAI / ChatGPT backend

turn model = newapi/qwen3.8
        → 本地 Router
        → NewAPI
```

### 必须设计的接口

至少需要解决：

1. logical model ID 的解析；
2. 官方 slug 与 namespaced slug 的区分；
3. per-turn backend selection；
4. Router 地址和 local capability token 的发现；
5. Custom turn 的请求格式；
6. streaming event 回传；
7. cancellation / timeout；
8. retry 与错误映射；
9. Thread history 的可见文本和 tool state 处理；
10. 官方 turn 不经过 Custom adapter。

### 明确禁止

不要通过以下方式实现：

```text
修改 model_provider
修改 Thread provider
复制 auth.json
让 Router 接管所有官方请求
把官方 OAuth 转发给第三方
```

### Phase 1 完成标准

必须有真实测试证明：

```text
同一个 Thread
    Official turn
    Custom turn
    Official turn
```

并且三轮分别到达正确 backend。

---

## Phase 2：Router 安全边界

### 目标

使本地 Router 只能被授权的 Codex Core/Bridge 调用。

### 必须完成

1. 安装时生成随机 capability token；
2. Router 强制校验：
   ```http
   Authorization: Bearer <local-router-token>
   ```
3. 校验 loopback Host；
4. 校验 Origin 或使用更安全的 IPC；
5. 强制 JSON Content-Type；
6. 限制 body size；
7. 不记录 credential；
8. 不回显官方 Authorization；
9. 非 loopback Provider 强制 HTTPS；
10. 明确 redirect / proxy / DNS 风险。

### 推荐实现

优先评估：

```text
Unix domain socket
Windows named pipe
```

如果必须使用 TCP loopback，则至少使用随机 token，并让 Core/Bridge 自动发现端口和 token，而不是固定公开端口。

本轮已实现其中的 Linux V1 子集：loopback-only、动态端口、随机 capability token、0600 endpoint file、JSON body 限制和未授权拒绝。Host/Origin 强化、Unix socket、Windows named pipe、完整生命周期仍未完成，不能把此子集描述成完整 IPC 安全方案。

---

## Phase 3：完成协议双向归一化

### 当前问题

`crates/router/src/lib.rs` 当前只把请求转发给 upstream，响应 body 和 SSE 原样透传。

### 必须完成

对于 Responses Provider 与 Chat Completions Provider，至少需要覆盖：

- JSON response；
- streaming SSE；
- response ID；
- usage；
- finish reason；
- tool call；
- function result；
- error schema；
- cancellation；
- partial output；
- reasoning / summary（如果 Provider 支持）。

### V1 可接受的限制

如果完整双向转换暂时无法可靠实现，V1 可以明确限制为：

```text
只允许 Responses-compatible Provider
```

但不能继续把“请求转换成功”描述成“协议兼容完成”。

---

## Phase 4：建立明确的 Context Boundary

### 需要保留

- 用户可见文本；
- assistant 可见文本；
- 可迁移的 function call；
- function result；
- 必要的 tool call/result 关联关系；
- 可被目标 Provider 理解的图片和文件输入。

### 默认不可迁移

- encrypted reasoning；
- 官方 response ID；
- Codex hosted tool opaque state；
- provider-specific metadata；
- 不能被第三方解释的 compaction 数据；
- 官方专属 web/computer/file-search state。

### 行为要求

对于不可迁移内容，必须二选一：

```text
明确拒绝本次切换并说明原因
```

或：

```text
显式降级并记录用户可见的 Context Boundary
```

不得静默丢弃后继续宣称完全兼容。

### 必须有的测试

```text
Official → Custom → Official
```

覆盖：

- 普通文本；
- reasoning summary；
- tool call/result；
- 图片；
- 文件；
- compaction；
- streaming；
- 中断后继续；
- Provider 错误恢复。

---

## Phase 5：Catalog 与模型能力安全化

### 必须完成

1. 保持官方 Catalog 原样；
2. Custom model 使用稳定 namespaced slug；
3. Custom 条目采用显式字段 allowlist；
4. 不要无条件 clone 第一个官方模型的所有未来字段；
5. 不要错误宣传 hosted tools、Web Search、Image Generation 等能力；
6. 对每个 Custom model 明确：
   - context window；
   - reasoning；
   - tools；
   - images；
   - files；
   - streaming；
   - Responses / Chat Completions；
7. Catalog schema 与 Codex 版本不兼容时拒绝启用。

相关代码：

```text
/home/lvziw/项目/codex模型切换器/crates/catalog/src/lib.rs
```

---

## Phase 6：Registry reload 与 Router 生命周期

本轮已落地 `RouterSupervisor` 的启动、健康检查、reload、受保护 shutdown、优雅停止和进程退出兜底；面板也提供了 Router 刷新入口。仍需在真实安装环境验证崩溃拉起、Provider 修改后的实际 generation 一致性，以及 Custom 不可用时官方模型不受影响。

### 必须完成

1. Provider/Model 修改后，运行中的 Router 能够 reload；
2. Catalog 与 Router 使用同一 Registry generation；
3. Router 崩溃可以被 supervisor 拉起；
4. Codex 能检查 Router health；
5. Router 不可用时，官方模型仍然可用；
6. Custom model 明确显示不可用，而不是让官方 Core 卡死；
7. 端口、token、进程状态不写入不安全的普通日志。

---

## Phase 7：安装、升级、Repair、卸载

本轮已落地 Linux CLI/面板安装脚本、用户级 desktop/autostart 项、可回滚 Codex integration、先停 Router 再清理状态的卸载流程，以及 Provider registry/keyring 清理失败时的恢复保护。真实目标机安装包、升级和卸载验收仍未执行。

### 安装

Linux 需要至少支持：

- CLI 安装；
- Router service 或受控自动启动；
- 初始化状态目录；
- 生成 local capability token；
- health check；
- 安装结果验证。

Windows 最终需要：

```text
Setup.exe
```

### Upgrade / Repair

必须有：

```text
Codex version gate
Catalog schema gate
Core/Bridge compatibility gate
Router health gate
失败自动禁用 integration
恢复原生 Codex
保留诊断信息
```

### Uninstall

卸载后必须保证：

```text
model_provider = openai
ChatGPT 登录正常
旧 Thread 正常
官方模型正常
```

允许的结果只有：

```text
Custom models 消失
```

不得删除或破坏用户原有的：

- `auth.json`；
- 非本项目的 `model_catalog_json`；
- Codex profiles；
- Thread 数据；
- 用户配置。

---

## 5. F1-F5 验收门槛

没有完成以下门槛，不得宣称最终产品完成。

### F1：真实 Desktop UI / Bridge

```text
Codex Desktop 原生 Model Picker
        ↓
看见 Custom model
        ↓
点击 Custom model
        ↓
真实 turn 进入本地 Router
```

要求：

- 不依赖本项目 GUI 进行日常切换；
- 不依赖脆弱 DOM patch，除非有书面决策；
- 记录真实 UI、Bridge、Core、Router、Provider 的调用证据。

### F2：双 Thread / Provider 隔离

同时运行：

```text
Thread A：官方模型
Thread B：Custom 模型
```

证明：

- 两个 Thread 的 Codex provider 都是 `openai`；
- ChatGPT Account 不变；
- Thread 不迁移；
- Custom 配置不会污染官方 Thread；
- 一条 Thread 的切换不会影响另一条 Thread。

### F3：同一 Thread 逐轮切换

必须证明：

```text
Official → Custom → Official
```

并满足：

- 同一 Thread；
- 不重启 Codex；
- 不新建 Thread；
- 不重新登录；
- 官方请求走官方 backend；
- Custom 请求走 Router；
- 下一轮立即生效。

### F4：真实 Context Boundary

必须覆盖：

- 文本历史；
- reasoning；
- tool call/result；
- 图片；
- 文件；
- compaction；
- 中断恢复；
- streaming；
- 错误恢复。

必须记录哪些内容保留、哪些内容拒绝或降级。

### F5：Tools / Hosted Capabilities

分别验证：

```text
shell
file
git
MCP
Skills
hosted tools
web search
image input
reasoning
```

对于第三方不支持的能力，必须是：

```text
明确禁用
明确拒绝
明确降级
```

不能通过 Catalog 字段伪装成已支持。

---

## 6. 证据分类规则

后续报告必须区分以下证据，不能混为一谈：

| 证据等级 | 能证明什么 | 不能证明什么 |
|---|---|---|
| 源码检查 | 某路径是否存在、某配置是否被修改 | 不能证明真实运行成功 |
| 单元测试 | 函数级行为和 mock 协议 | 不能证明 Codex caller 接入 |
| CLI Catalog 实验 | 模型是否出现在原生列表 | 不能证明 Custom turn 到达 Router |
| 独立 Router 测试 | Router 的直接请求转换和 credential 替换 | 不能证明 Codex 会调用 Router |
| 真实 Codex CLI | CLI caller 的实际行为 | 不能代表 Desktop |
| Desktop / Bridge | Desktop 原生 UI 和 caller | 不能自动代表 Remote Host |
| Remote app-server | 远程 Host 的模型列表和请求路径 | 不能替代真实 Provider E2E |
| Provider E2E | 第三方真实 API 行为 | 不能证明官方 Thread 未被破坏 |
| 生产验收 | 真实部署环境可用性 | 不能替代源码安全审计 |

所有最终结论都必须标记：

```text
PASS
FAIL
NOT RUN
NOT PROVEN
```

---

## 7. 当前已执行验证记录

本轮实现后的本地 workspace 验证：

```text
cargo fmt --all -- --check                         PASS
cargo fmt --manifest-path apps/panel/src-tauri/Cargo.toml -- --check PASS
cargo test --workspace --all-targets               PASS (36 tests passed)
cargo clippy --workspace --all-targets --all-features -- -D warnings PASS
cargo build --release --locked --package codex-mp-cli PASS
node --check apps/panel/main.js                   PASS
bash -n installer/install-linux.sh installer/uninstall-linux.sh PASS
python3 -m json.tool apps/panel/src-tauri/tauri.conf.json PASS
CLI help/uninstall temp-dir smoke                 PASS
Tauri panel cargo check                           NOT PROVEN (已尝试，缺少 pkg-config/GTK/WebKit 宿主依赖)
```

本轮 patched upstream Core 验证（上游 commit `73a1148c9c775c2a4616ce5096291740a00ed68a`）：

```text
CARGO_TARGET_DIR=/home/lvziw/codex-rs-target \
OPENSSL_DIR=/usr \
OPENSSL_LIB_DIR=/usr/lib/x86_64-linux-gnu \
OPENSSL_INCLUDE_DIR=/usr/include \
cargo check -p codex-core                    PASS

CARGO_TARGET_DIR=/home/lvziw/codex-rs-target \
OPENSSL_DIR=/usr \
OPENSSL_LIB_DIR=/usr/lib/x86_64-linux-gnu \
OPENSSL_INCLUDE_DIR=/usr/include \
cargo test -p codex-core logical_routing   PASS (4 tests passed)
```

补丁与应用脚本验证：

```text
sha256sum -c patches/codex/73a1148c9c775c2a4616ce5096291740a00ed68a/SHA256SUMS PASS
git apply --check（临时 clean worktree）       PASS
apply.sh 二次执行（already applied）             PASS
git diff --check（patched upstream）             PASS
release codex-mp Router smoke                   PASS（healthz、401、0600、token 不入日志）
```

已有或仍未证明的产品级项目：

```text
本地 route 判定与 Router mock                   PASS
Router capability token / 0600 endpoint          PASS
真实 Codex CLI/app-server custom turn            NOT PROVEN
Core/app-server → Router 实际 HTTP 数据面        NOT PROVEN
同 Thread Official → Custom → Official           NOT PROVEN
Desktop UI / Bridge                              NOT RUN
Remote Desktop → Remote Host                     NOT RUN
真实 NewAPI/OpenRouter Provider E2E              NOT RUN
F1                                               NOT PROVEN
F2                                               NOT PROVEN
F3                                               NOT PROVEN
F4                                               NOT PROVEN
F5                                               NOT PROVEN
Linux source-level Upgrade / Repair / Uninstall  PASS
真实目标机安装/升级/卸载                         NOT RUN
```

---

## 8. 后续实现的停止条件

出现以下任一情况时，必须停止编码并重新报告，不得自行绕过：

1. 需要修改 `model_provider`；
2. 需要创建新的 Codex Provider；
3. 需要修改或复制 `auth.json`；
4. 需要让 Custom model 使用 Codex API mode 替代 ChatGPT mode；
5. 需要迁移 Thread；
6. 需要重启 Codex 才能切换模型；
7. 只能通过把所有官方请求转发给第三方来实现；
8. 无法保证官方 Authorization 不进入第三方请求；
9. 无法定义 Context Boundary 却准备静默丢弃数据；
10. 只能通过桌面 DOM 假装模型已经接入；
11. 只能证明 Catalog 可见，无法证明真实 turn 路由。

---

## 9. 完成定义

只有同时满足以下条件，最终结论才可以从 `FAIL` 变更为 `PASS WITH CHANGES` 或 `PASS`：

1. P0 per-turn logical routing 已实现；
2. Codex provider/thread/auth 不变量通过真实运行验证；
3. F1-F5 全部完成；
4. 至少一个真实 Responses Provider 和一个真实 Chat Completions Provider 完成 E2E，或明确限制为 Responses-only；
5. Official → Custom → Official 同 Thread 通过；
6. Remote Host 场景通过；
7. Upgrade/repair/uninstall 通过；
8. 没有未解释的 credential 泄漏路径；
9. 自动化测试不再静默跳过关键验证；
10. 有可追踪的 commit、版本、构建产物和验收记录。

在此之前，正确结论仍然是：

```text
总体架构判定：FAIL
```

---

## 10. Luna 新对话交接说明

本节是给下一位实现模型的执行交接，不是新的产品需求，也不改变本文前述结论。

### 10.1 必须先读的文件

进入仓库后，首先阅读：

```text
/home/lvziw/项目/codex模型切换器/docs/DEVELOPMENT_REMEDIATION_PLAN.md
```

然后检查以下关键实现文件和当前工作区状态：

```text
/home/lvziw/项目/codex模型切换器/crates/integration/src/lib.rs
/home/lvziw/项目/codex模型切换器/crates/router/src/lib.rs
/home/lvziw/项目/codex模型切换器/crates/router/src/compatibility.rs
/home/lvziw/项目/codex模型切换器/crates/catalog/src/lib.rs
/home/lvziw/项目/codex模型切换器/crates/cli/src/main.rs
/home/lvziw/项目/codex模型切换器/docs/codex-integration.md
```

可对照的上游审计副本和 commit：

```text
/tmp/openai-codex-audit-1788958875
73a1148c9c775c2a4616ce5096291740a00ed68a
```

### 10.2 当前实现边界

本轮已落地并有自动化或构建证据的部分：

- `model_provider` 保持 `openai`；
- 不创建新的 Codex Provider；
- 不修改或迁移 ChatGPT OAuth、`auth.json` 或 Thread Provider；
- API Key 通过 keyring 与普通配置隔离；
- 官方 Catalog 与 Custom namespaced model 可以显示；
- app-server `model/list` 可以返回 Custom model；
- 独立 Router 基础路由、capability token 和 mock 测试已经通过；
- manifest/restore 用户修改保护已经存在；
- 上游固定 commit 的 patched Core 已接入 `ModelClientSession::stream()`，并通过 `codex-core` check 与 logical-routing test；
- 可复现 patch、应用脚本、版本绑定和构建说明已写入 `patches/codex/73a1148c9c775c2a4616ce5096291740a00ed68a/`。

仍未通过真实产品验收的关键部分：

```text
Codex 原生 Model Picker
        ↓
Codex Core/app-server per-turn logical model
        ↓
Official 原生 backend 或 Local Router
        ↓
第三方 Provider
```

目前尚不能声称已完成：

- 真实 Custom turn E2E；
- 同一 Thread 的 `Official → Custom → Official`；
- Desktop UI / Bridge；
- Remote Desktop → Remote Host；
- 真实第三方 Provider E2E；
- 完整 Context Boundary；
- Tools、Hosted Capabilities、流式事件和错误语义；
- 真实目标机 Router 生命周期、reload、故障恢复验收；
- Windows 安装器及完整 Upgrade / Repair / Uninstall。

### 10.3 下一步强制顺序

P0 已选择并落地为**受控 patched Codex Core**。下一步不得回退到 Catalog-only 或独立 GUI；应围绕以下真实链路补齐验证和剩余 Phase：

```text
Codex turn/start
    → ModelInfo.slug
    → patched ModelClientSession::stream()
    → Official backend 或 Local Router
```

实现要求：

- Codex 对外仍保持 `provider = openai`；
- Official model 继续走原生 OpenAI/ChatGPT backend；
- 只有 Custom namespaced model 进入 Local Router；
- 路由必须发生在真实 Core/app-server turn 数据面；
- 不能只把模型加入 Catalog 后宣称完成；
- 不能依赖独立 GUI、DOM、重启、重新登录或新建 Thread；
- 如果需要外部上游补丁，必须在当前仓库留下可复现的补丁、版本绑定、构建和升级说明。

P0 接通并验证后，继续按本文 Phase 2 至 Phase 7 处理 Router 安全、协议转换、Context Boundary、Catalog 能力、生命周期和安装维护。

### 10.4 不可破坏的不变量

实现期间始终遵守 I1-I7：

- 不修改 `model_provider`；
- 不修改、复制或泄漏 `auth.json` / ChatGPT OAuth；
- 不迁移 Thread Provider；
- Official 请求不得进入第三方兼容层；
- Custom 请求才进入 Router；
- API Key 不得进入普通文件、Git、日志或错误消息；
- 日常切换不得依赖重启 Codex。

如果某条路径必须破坏上述不变量，必须停止该方向并报告，不能自行改变架构或用假路由绕过。

### 10.5 工作区和证据要求

开始前先检查 `pwd`、`git status --short` 和文件清单。当前仓库没有稳定提交历史，不能执行 `git clean`、`git reset --hard` 或删除未知未跟踪文件。

实现时必须写入真实代码、测试和文档，而不是只留下 TODO。每项验证都要单独标记：

```text
PASS / FAIL / NOT RUN / NOT PROVEN
```

Catalog 可见、Router 独立测试通过、源码接口存在，都不能替代真实 Codex turn、Desktop、Remote 或真实 Provider 验收。最终报告必须列出修改的绝对路径、实际执行的命令、真实调用链证据、剩余阻塞以及 F1-F5 状态；无法执行的项目必须明确写为 `NOT RUN` 或 `NOT PROVEN`。
