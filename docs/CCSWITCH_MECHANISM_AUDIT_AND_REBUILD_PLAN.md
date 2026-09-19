# Codex OmniBridge Linux CLI：机制审计、CC Switch 协议解构与重建方案

> 状态：实现前的架构基线与验收合同
>
> 日期：2026-09-13

> 验收更新：2026-09-14（补充 stock CLI 与 Windows VM 证据）
>
> 本项目基线：`9fda9778d5a31b4c7fdf6fd2243aa6122d606792`
>
> OpenAI Codex 审计基线：`ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8`
>
> CC Switch 审计基线：`1d5d90f4aba88447d422a16cdec5282ec5331fd7`（v3.20.3）

## 0. 文档定位

本文不是现状说明，也不是在现有补丁上继续堆修补的清单；它是下一轮开发的唯一机制基线，目标是同时实现：

1. 保留官方 ChatGPT/Codex 订阅登录，不要求用户把官方 OAuth 凭据改成 API Key；
2. 在同一个 Codex 会话中，通过模型选择器在官方模型和自定义模型之间切换；
3. 自定义上游可以是原生 Responses API，也可以是旧式 Chat Completions API；
4. 远端 Linux 主机安装 OmniBridge 并配置自定义模型后，本地 Codex Desktop 通过 Remote Control 能看到远端目录并真正调用这些模型；
5. 官方请求、第三方密钥、历史、工具调用和推理内容之间不存在越权串流；
6. 默认追随 stock Codex，而不是长期维护一个极易失效的 Codex Core 私有分叉。

本文会取代 `docs/DEVELOPMENT_REMEDIATION_PLAN.md` 中与“官方流量永不进入本地网关”“依赖固定 Core 补丁进行每轮路由”“只写 `model_catalog_json` 即完成集成”有关的旧结论。旧文档仍可作为历史审计记录，但不得作为下一轮实现合同。

本文中的“Responses”均指官方路径 `/responses`；`/response` 不是 OpenAI 的标准端点，不应作为内部契约。若用户配置里出现 `/response`，doctor 应给出可操作错误，而不是静默猜测。

## 1. 结论先行

### 1.1 当前版本为什么“目录里可能看得到，但实际上不能用”

当前实现同时存在四个互相叠加的断点：

- **目录断点**：自定义模型 JSON 是手写的旧 schema，缺少当前 Codex 必填的 `shell_type`，也没有 `model_messages`；最新 Codex 不能可靠加载它，即便只做 JSON 语法校验会通过。
- **选路断点**：stock Codex 的一条 thread 固定 `model_provider`，`turn/start` 只允许换 `model`；当前项目却把“官方模型走原生 provider、自定义模型走另一 provider”的切换责任放在一个已经过期的 Core 补丁中。
- **协议断点**：现有 Router 的 Responses↔Chat 转换只覆盖纯文本的很小子集；工具、并行工具、reasoning、历史续接、非流式返回、完整 SSE 生命周期、compact、search、images 都不完整。
- **远端拓扑断点**：Remote Control 实际使用远端 app-server 的配置和模型目录；给本地 Desktop 替换 patched runtime 不能让它正确调用远端模型，而当前 Router 又是短生命周期 wrapper 子进程，不适合作为远端常驻依赖。

因此，当前版本的本地单元测试通过并不与“现在没法正常使用”矛盾：测试验证的是本项目自定义 JSON 形状和 mock 路由，不是目标 Codex 对目录的真实反序列化，也不是官方 OAuth、自定义工具循环或远端 Desktop 的端到端调用。

### 1.2 采用的架构决策

默认架构改为：**stock Codex + 单一 OmniBridge Responses provider + 稳定本地网关 + 按逻辑模型精确选路**。

- Codex 中只配置一个承载全部可切换模型的 provider，例如 provider id 为 `omnibridge`；
- 该 provider 的 `name` 使用 Codex 能识别的 OpenAI 语义，`requires_openai_auth = true`，`wire_api = "responses"`，`supports_websockets = false`；
- 官方模型和自定义模型都出现在同一份、与目标 Codex schema 匹配的目录中；
- thread 的 provider 始终是 `omnibridge`，每个 turn 只换 `model`，网关再根据精确的逻辑模型 ID 决定走官方订阅还是第三方上游；
- 官方分支透传 Codex 已持有的 ChatGPT 授权和账户上下文，自定义分支必须先剥离官方授权，再注入该 provider 自己的凭据；
- Remote Control 场景把配置、目录、Router、凭据和历史账本全部放在远端主机，本地 Desktop 使用 stock runtime，只承担远端 app-server 的 UI/传输。

这是能在当前 stock Codex 约束下同时满足“同 thread 切模型”和“Remote Desktop 看见远端模型”的最短稳定路径。

### 1.3 必须坦白的信任边界变化

要在不修改 Codex Core 的前提下让同一 thread 在官方与自定义模型间切换，官方请求也必须经过本机 OmniBridge。因此 OmniBridge 会在内存中看到官方 bearer token 和账户标识；它必须是仅监听 loopback、不开启请求体/授权日志、严格过滤头、可审计的本机可信组件。

如果产品要求“官方 OAuth 的每一个字节都绝不能进入 OmniBridge 进程”，那么 stock Codex 下不能同时获得无缝同-thread切换；只能继续维护一个上游 Core 补丁，使 Core 在发送前分流。本文把这种方式保留为高维护成本的备用模式，不作为默认实现。

## 2. 审计方法与证据边界

### 2.1 实际审计对象

- 当前工作树中的 Linux CLI、Core、Catalog、Integration、Router、Manager、Credentials 和 Desktop adapter；
- 当前项目固定的 Codex 补丁及构建脚本；
- OpenAI Codex 最新源码中 provider、model catalog、thread/turn、app-server model list、remote-control 和 compaction 的真实契约；
- CC Switch v3.20.3 中 Codex 配置、官方授权透传、Responses↔Chat、SSE、工具、reasoning、历史补全、compact/search/images 和 provider routing 的实现；
- OpenAI 官方 Responses 与 Chat Completions API 参考。

### 2.2 本轮已执行验证

| 检查 | 结果 | 说明 |
|---|---|---|
| `cargo fmt --all -- --check` | PASS | 2026-09-13 当前工作树 Rust 格式检查通过 |
| `cargo test --workspace --all-targets --jobs 1` | PASS | 220 个本地测试全部通过（0 failed）；本地测试本身不替代真实 provider/Remote Control E2E |
| `cargo clippy --workspace --all-targets --jobs 1 -- -D warnings` | PASS | 当前工作树静态 lint 通过 |
| `git diff --check` | PASS | 当前工作树无 whitespace error |
| workspace release build | PASS | `cargo build --locked --release --workspace --jobs 1` 完成；release `codex-mp` 可执行 |
| legacy 固定补丁应用检查 | PASS | 对 pinned `73a1148c9c775c2a4616ce5096291740a00ed68a` 的上游源码完成 apply/reverse-check；该 patched Core 仍不是默认路径 |
| 默认构建路径 | PASS | build/install 默认使用 stock Codex；旧 patched Core/Desktop 仅保留 legacy/experimental 路径 |
| 最新 stock Codex catalog/app-server contract | PASS | clean stock HEAD `ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8` 独立构建并启动；真实 stock app-server `initialize`、`model/list`、`thread/start` 成功 |
| 指定临时 `--registry` 后运行 sync 的隔离性 | PASS | sync 与 stock E2E 都使用临时 registry/CODEX_HOME/XDG；generated config/catalog/manifest/capability 未写入全局目录 |
| 固定 CC Switch commit 移植与 MIT notice | PASS | commit `1d5d90f4aba88447d422a16cdec5282ec5331fd7` 五个移植源文件 SHA-256 与上游逐一相等；174 项 bridge tests、LICENSE 与 `THIRD_PARTY_NOTICES.md` 均在仓库 |
| 官方 OAuth 到受控 fixture 的安全透传 | PASS | stock E2E 官方记录有 bearer/account，custom 记录为独立第三方 bearer；capability 未转发；只输出 hash |
| 官方订阅真实网络调用 | NOT RUN | 本轮未向 ChatGPT 官方 backend 发送请求；受控 fixture 仅证明 OAuth 透传和隔离策略 |
| 第三方 provider 真实调用 | NOT RUN | 本轮没有向真实 provider 发送请求 |
| 本地 stock app-server model/list | PASS | `scripts/stock-omnibridge-e2e.py` 用独立 stock app-server 返回并断言 `gpt-5.5`、`newapi/qwen3.8`；thread provider 为 `omnibridge` |
| 本地 stock CLI custom turn | PASS | stock `codex exec` 在隔离 `CODEX_HOME` 中完成 `newapi/qwen3.8` turn；Provider 收到重写后的 `qwen3.8`、Bearer 与流式请求，未收到 Router capability，CLI 退出码为 0 |
| 本地 official→custom→official Router E2E | PASS（受控 fixture） | release Router + stock app-server 同一 thread 三轮均 `completed`；上游顺序 `gpt-5.5 → qwen3.8 → gpt-5.5`，并覆盖 official/custom compaction、reasoning、history input 和 auth hash 隔离 |
| Responses↔Chat/工具/reasoning/SSE/history 单元闭包 | PASS | bridge 174 项、router 12 项；包含 Chat non-stream/stream、工具/并行项、reasoning、UTF-8/SSE、history boundary、压缩请求体和安全头断言 |
| systemd service 内容/脚本静态验收 | PASS | shell `bash -n` 通过；unit 含 loopback 进程、`Restart=on-failure`、`UMask=0077`、journald、NoNewPrivileges、受限地址族 |
| systemd user service 实机启动/重启 | NOT PROVEN | `systemd-analyze verify` 仅因本机尚未安装 `%h/.local/bin/codex-mp` 报缺少可执行文件；未在真实安装目录启动 unit |
| Windows PowerShell 语法/运行验收 | PASS | Windows 11 VM 通过 PVE Guest Agent 执行最新 build、installer、uninstaller 三份脚本的 PowerShell parser；真实 patched Codex Windows artifact 仍未运行 |
| 本地 Desktop 真实交互 | NOT RUN | 本轮未替换或重启 Desktop runtime；legacy adapter 已明确标为 experimental |
| Remote Control E2E | NOT RUN | 当前没有可用的本地 stock Desktop↔远端配对会话；未冒充 Remote Control 通过本地 app-server probe |
| headless keyring 实机 | NOT PROVEN | 默认 keyring 失败关闭和显式 0600 file backend 有单测/E2E；未在无 Secret Service 的 systemd credential 环境实机验收 |
| SQLite/WAL history restart | NOT RUN | 当前交付使用受限内存 ledger；未把可选 SQLite/WAL 作为本轮实现范围 |

本地 stock E2E 的可复核命令是：
`python3 scripts/stock-omnibridge-e2e.py --repo "$PWD" --registry /tmp/omnibridge-stock-e2e.final.vcHe7C/codexhome/providers.json --catalog /tmp/omnibridge-stock-e2e.final.vcHe7C/codexhome/models.json --capability /tmp/omnibridge-stock-e2e.final.vcHe7C/codexhome/router-capability --auth "$HOME/.codex/auth.json" --router-bin "$PWD/target/release/codex-mp" --app-server-bin /home/lvziw/codex-stock-audit-target/release/codex-app-server --cli-bin /home/lvziw/codex-stock-audit-target/release/codex`；本轮退出码为 0，三轮 app-server turn 和一轮 stock CLI turn 均完成，脱敏记录中 `auth_unchanged=true`，custom Provider 收到独立第三方 Bearer hash 且未收到 capability。

隔离 `sync` 的可复核证据为 `/tmp/codex-mp-sync-final.log` 与目录 `/tmp/codex-mp-sync-final.fIqX9L`：`config.toml`、`models.json`、`integration.json`、`router-capability` 均只写入该临时目录并为 0600；命令输出明确 `Codex provider was not changed; OAuth credentials were not read.`。

`PASS` 只适用于相应检查，不能外推为产品可用。

## 3. 不可妥协的产品与安全合同

### 3.1 用户可见合同

1. 官方登录仍由 Codex 官方流程管理。安装、同步、路由与卸载流程不读写 `auth.json`。
   账号管理是**独立的可选功能**：它读取 `auth.json` 以显示当前登录账号，并且只在你主动
   切换账号时备份并改写它。托管账号的 OAuth 令牌存储在系统钥匙串中，不写入
   `accounts.json`，也绝不随请求转发给任何第三方 Provider。
2. 模型列表同时显示官方模型与启用的自定义模型；重名必须在 sync 阶段拒绝。
3. 新 thread 使用 `omnibridge` provider 后，可在后续 turn 直接换官方或自定义模型。
4. 切换模型不能清空用户可见历史，也不能把上一上游的私有 response ID 发送给另一上游。
5. 工具能力不允许“目录里声称支持但协议层会丢掉”；不能证明完整生命周期时必须从目录隐藏或 fail closed。
6. 远端模式中，本地 Desktop 显示的是远端 app-server 的模型目录，实际请求也只在远端发出。

### 3.2 安全合同

1. Router 只绑定 `127.0.0.1`/`::1`，拒绝非 loopback Host，并使用独立能力头，不占用 `Authorization`。
2. 官方路由只能透传官方入站凭据，绝不能使用任何第三方 provider 的密钥，也绝不能进入 failover。
3. 自定义路由必须先删除 `Authorization`、`ChatGPT-Account-Id`、所有不应离开官方边界的会话/设备头，再按明确 auth strategy 注入第三方凭据。
4. 日志禁止记录 token、API key、完整请求体、完整工具参数、图片 data URL、用户提示或 reasoning；debug dump 必须显式短时开启并做结构化脱敏。
5. provider、model、route、catalog profile、auth strategy 必须是同一份注册表的原子快照，不能分别热读后产生混合版本。
6. 所有配置改写使用锁、期望哈希和可恢复快照；目标文件是 symlink、目录、权限异常或并发变化时 fail closed。
7. 密钥落盘必须由用户显式选择；keyring 失败不能静默降级为明文文件。

### 3.3 协议合同

1. Codex 入口以 Responses 为主；Chat Completions 只是上游 adapter，不成为 Codex provider 的 `wire_api`。
2. 请求转换和响应转换必须成对实现；禁止只转换请求、把 Chat 非流式响应原样冒充 Responses。
3. SSE 必须生成完整、有序且索引一致的 Responses 事件序列，并正确处理 UTF-8 分片、多行 `data:`、错误、usage、finish reason 和连接取消。
4. 所有 function/custom/namespace/tool_search 调用及其 output 必须保留稳定 ID 和顺序；任何无法表示的 item 都明确报 context-boundary 错误。
5. `previous_response_id` 只能在同一后端、同一账户、同一协议和同一路由代际内透传；其他情况必须通过历史账本水化为显式 input。

## 4. 当前机制的真实调用链

### 4.1 当前本地 CLI 路径

```text
registry.json
   │
   ├─ catalog: `codex debug models --bundled` + 手写 custom JSON
   │                                      └─ 写 models.json
   │
   ├─ integration: 仅给 ~/.codex/config.toml 写 model_catalog_json
   │
   └─ manager: 临时端口启动 Router
                      │
patched Codex Core ───┴─ 看到 model 含 `/` 才读取 endpoint file 并分流
       │
       ├─ 无 `/`：原生官方路径
       └─ 有 `/`：Router -> provider
```

这条链的根本问题不是某一个 if 写错，而是把 `model_provider` 的约束绕过工作放在私有 Core patch 里。补丁固定在旧 commit，最新源码已经改动；即使重新补上，它也只覆盖 `stream()` 路径，并把 endpoint 在 client 构造时捕获，无法为稳定 Remote Control 生命周期提供可靠保证。

### 4.2 当前远端设想与真实拓扑的差异

当前 Desktop adapter 会替换本地用户级 standalone entrypoint，并启动 patched app-server；这只对“本地 Desktop 调本地 app-server”有意义。

Remote Control 的真实链路是：

```text
本地 Codex Desktop
      │ 配对/传输
      ▼
远端 `codex remote-control` / 远端 app-server
      │
      ├─ 读取远端 ~/.codex/config.toml
      ├─ 返回远端 model/list
      ├─ 使用远端 ChatGPT 登录状态
      └─ 从远端发起 Responses 请求
```

因此：

- 自定义目录必须安装在远端；
- Router 必须在远端、先于 app-server 启动并持续存活；
- 第三方密钥必须在远端；
- 本地 Desktop 不应为了远端模型而替换 binary；
- 验收必须证明本地 UI 选择模型后，远端 Router 的 route audit 与上游请求一致。

### 4.3 当前代码证据地图

| 组件 | 当前责任 | 审计定位 |
|---|---|---|
| `crates/core/src/lib.rs` | registry、provider/model schema、模型选路、路径和原子替换辅助 | `resolve_logical_model_route` 使用模型命名启发式；ProviderConfig 无 auth/header/dialect/timeout 合同 |
| `crates/catalog/src/lib.rs` | 从 bundled catalog 合并 custom entry | `custom_entry` 忽略模板并手写旧 schema；测试没有走目标 Codex 类型 |
| `crates/integration/src/lib.rs` | 写 catalog 并修改 Codex TOML | 只管理根级 `model_catalog_json`；路径从全局 default registry 派生；逐行编辑 TOML |
| `crates/router/src/compatibility.rs` | 请求协议兼容 | 删除 stateful 字段，Chat 转换只覆盖 message 子集，流式方向还主动拒绝 |
| `crates/router/src/lib.rs` | HTTP 入站、选路、上游和临时 SSE 转换 | 官方返回 501；所有第三方只用 Bearer；只转 Accept；Chat non-stream 原样返回 |
| `crates/manager/src/lib.rs` | 拉起 Router 并管理 endpoint file | Router 依附 wrapper 生命周期；复用只看 `/healthz`；子进程诊断不足 |
| `crates/credentials/src/lib.rs` | env/keyring/file secrets | keyring 写失败会无提示落到 `.credentials`；文件更新非事务化 |
| `crates/desktop/src/lib.rs` | 替换本地 Desktop standalone runtime | 固定旧 Codex commit；对 Remote Control 的远端模型链路没有决定权 |
| `patches/codex/73a.../0001-per-turn-local-router.patch` | Core 内逐轮 model 分流 | 只覆盖部分 client path；custom compact 拒绝；对最新上游已不能应用 |
| `scripts/build-patched-codex.sh` | 构建固定 patched Codex | 对非固定 commit fail closed，形成长期上游合并负担 |

这张表也是实施时的导航表：不能只替换 Router 的转换函数而保留其余错误契约。

## 5. 最新 Codex 给出的硬约束

### 5.1 provider wire API 只剩 Responses

最新 Codex 的 `WireApi` 只接受 Responses；旧 `chat` 值会给出已移除错误。换言之，不能再靠 `wire_api = "chat"` 让 stock Codex 原生兼容旧上游，转换必须放在网关中。[OpenAI Codex provider 源码](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/model-provider-info/src/lib.rs#L61-L155)

OpenAI 官方接口也明确区分 `POST /responses` 与 `POST /chat/completions`；二者不是只改 URL 就等价，输入 item、工具、reasoning、续接和流式事件都不同。[Responses API](https://developers.openai.com/api/reference/cli/resources/responses/methods/create)；[Chat Completions API](https://developers.openai.com/api/reference/cli/resources/chat)

### 5.2 provider 是 thread 级，model 可以是 turn 级

`thread/start` 和 `thread/resume` 能指定 `model_provider`，但 `turn/start` 只有 `model`，没有 provider。也就是说，单纯把官方模型挂在 `openai`、自定义模型挂在 `custom`，不能通过 Desktop 的每轮模型选择实现同-thread跨 provider。[TurnStartParams](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/app-server-protocol/src/protocol/v2/turn.rs#L166-L229)；[ThreadStartParams](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/app-server-protocol/src/protocol/v2/thread.rs#L62-L71)

这也是为什么目标方案要让官方与自定义模型共享 `omnibridge` provider，再在其后按 `model` 选路。

### 5.3 catalog 是严格的版本化契约

当前 `ModelInfo` 含 `shell_type`、visibility、API 支持、priority、reasoning levels 等字段；本项目手写 entry 缺少当前必填字段。目录生成不能继续以“能 parse 成 `serde_json::Value`”作为成功标准。[ModelInfo](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/protocol/src/openai_models.rs#L402-L507)

同时，缺少 `model_messages` 时 Codex 无法得到该模型的 base instructions。自定义目录必须显式继承与目标 Codex 匹配的安全模板，或提供经过验证的完整字段集合；不能把未知字段随意置空。

### 5.4 Remote Control 的 model/list 来自远端 app-server

app-server 的 catalog processor 从它所在 runtime 的 thread manager 获取 supported models。因此在 Remote Control 中，本地 Desktop 展示的模型能力最终来自远端 app-server，而不是本地被替换的 catalog。[catalog processor](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/app-server/src/request_processors/catalog_processor.rs#L247-L294)

### 5.5 compact 和 WebSocket 不能忽略

Codex 对被识别为 OpenAI 的 provider 会启用远端 compaction 语义；网关必须处理 `/responses/compact`。同时，自定义 provider 应明确 `supports_websockets = false`，否则 stock Codex 可能先尝试 WebSocket，再降级到 SSE，导致延迟和难以诊断的失败。[provider compaction](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/model-provider/src/provider.rs#L348-L365)

## 6. 目标架构

```text
                         Remote Control 场景
┌──────────────────┐       ┌───────────────────────────────────────────────┐
│ Local Desktop    │◀─────▶│ Remote stock Codex app-server               │
│ stock runtime    │       │ provider = omnibridge, turn model 可变化      │
└──────────────────┘       └──────────────────────┬────────────────────────┘
                                                  │ POST /responses
                                                  │ x-codex-omnibridge-token
                                                  ▼
                              ┌────────────────────────────────────────────┐
                              │ OmniBridge Gateway (loopback systemd user)│
                              │                                            │
                              │  auth gate → exact route → profile         │
                              │       │              │                     │
                              │       │              ├─ protocol adapter   │
                              │       │              ├─ tool profile       │
                              │       │              └─ history ledger     │
                              └───────┼──────────────┼─────────────────────┘
                                      │              │
                       official route │              │ custom route
                                      ▼              ▼
                    chatgpt.com/backend-api/codex    Responses / Chat / later Anthropic
                    inbound OAuth + account context  provider-specific auth only
```

### 6.1 Codex provider 配置形态

安装器应通过语义 TOML 编辑生成类似以下配置；字段名必须由目标 Codex compatibility profile 验证，示例不是让实现者做字符串拼接：

```toml
model_provider = "omnibridge"
model_catalog_json = "/absolute/path/to/models.json"

[model_providers.omnibridge]
name = "OpenAI"
base_url = "http://127.0.0.1:PORT/v1"
wire_api = "responses"
requires_openai_auth = true
supports_websockets = false
http_headers = { "x-codex-omnibridge-token" = "LOCAL_CAPABILITY" }
```

注意：

- provider id 不能使用内建且不可覆盖的 `openai`；
- `name = "OpenAI"` 会触发 Codex 中部分名称驱动的 OpenAI 能力，这是有意行为，也必须被兼容性测试覆盖；
- 本机 capability 不是上游凭据，仍需 0600 权限并在转发前无条件删除；
- 若后续使用 `env_http_headers`，要证明 Desktop/remote-control 启动的 app-server 确实继承该环境变量，否则不能默认采用；
- 绝不修改 `auth.json`。

### 6.2 每次 `/responses` 的确定性算法

1. 验证 peer、Host、method、Content-Type、body limit 和独立 capability header；使用常量时间比较。
2. 解压受支持的 Content-Encoding，并以增量/有上限方式读取 JSON；记录 request id，但不记录内容。
3. 读取单一不可变 registry snapshot；按完整 `logical_model_id` 精确匹配，不使用“是否包含 `/`”推断。
4. 生成 `RouteDecision { generation, route_id, class, upstream_model, protocol, auth, tool_profile, dialect }`。
5. 根据 history ledger 判断 `previous_response_id` 能否原样透传；不能时水化显式 input。
6. 官方分支：验证当前确为 ChatGPT 登录、验证 account id 一致、删除 capability、按白名单透传官方所需头，保持官方 model slug，禁止 failover。
7. 自定义分支：先删除所有官方 auth/account/session 头和 capability，再按 `Bearer`、`x-api-key`、自定义头或命令凭据策略注入密钥，映射 upstream model。
8. 使用 route 对应的 request adapter；原生 Responses 也要做显式 dialect 正规化，Chat 则执行完整结构转换。
9. 发送请求时传播取消，应用连接/首字节/流空闲/总时限；只在明确安全的错误和幂等阶段重试。
10. 使用同一路由的 response adapter 返回 Responses；每个终止路径都写入结构化完成状态。
11. 成功时把 response/output/tool-call 元数据写入 ledger；失败或取消时写入可诊断但不含敏感内容的状态。

### 6.3 路由数据必须从“几个布尔值”升级为显式合同

建议的新模型（名称可调整，语义不可缩水）：

```rust
struct RouteProfile {
    route_id: String,
    route_class: OfficialSubscription | Custom,
    logical_model_id: String,
    upstream_model_id: String,
    upstream_base_url: Url,
    protocol: NativeResponses | ChatCompletions | AnthropicMessages,
    auth: OfficialPassthrough | BearerRef | ApiKeyRef | HeaderMapRef | Command,
    endpoint_policy: EndpointPolicy,
    request_dialect: RequestDialect,
    response_dialect: ResponseDialect,
    tool_profile: CodexToolProfile,
    reasoning_profile: ReasoningProfile,
    retry_policy: RetryPolicy,
    timeout_policy: TimeoutPolicy,
    capabilities: ProvenCapabilities,
}
```

`protocol`、`tool_profile`、`reasoning_profile` 与 catalog 能力必须在同一构建函数中产生，禁止 UI、catalog 和 Router 各自猜一次。

## 7. CC Switch 深度解构：哪些应直接移植，哪些不能照搬

CC Switch 采用 MIT License，可以复用其算法与代码，但直接复制代码时必须保留版权和许可声明，并在本项目新增 `THIRD_PARTY_NOTICES.md`，记录来源仓库、固定 commit、文件和本地改动。[CC Switch LICENSE](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/LICENSE)

### 7.1 应直接移植的协议核心

| CC Switch 源文件 | 已解决的问题 | 本项目目标位置 | 策略 |
|---|---|---|---|
| `proxy/providers/transform_codex_chat.rs` | Responses input/instructions 转 Chat messages；function、custom、namespace、tool_search；tool outputs；媒体；非流式 Chat→Responses | `crates/protocol-bridge/src/chat/*` | 以固定 commit 整体移植并拆模块，保留原测试；不要重写一个“简化版” |
| `proxy/providers/codex_chat_common.rs` | 多种 reasoning 字段/dialect 抽取与正规化 | `crates/protocol-bridge/src/reasoning.rs` | 移植后增加 provider profile 驱动测试 |
| `proxy/providers/codex_responses_sse.rs` | Responses SSE 事件对象、ID、序号和生命周期构造 | `crates/protocol-bridge/src/responses_sse.rs` | 直接复用事件模型，统一非流式与流式 item 类型 |
| `proxy/providers/streaming_codex_chat.rs` | Chat chunk 状态机、工具参数累积、finish、usage、UTF-8/多行 SSE、丢工具 fail closed | `crates/protocol-bridge/src/chat/stream.rs` | 整体移植，接入本项目 cancel/backpressure 和错误类型 |
| `proxy/providers/codex_chat_history.rs` | 记录 response，利用 `previous_response_id` 补齐 function output 前史 | `crates/history-ledger` | 复用语义，不照抄只保留 512 条的全局内存限制；增加 route/account/thread 隔离与可选持久化 |
| `proxy/providers/codex.rs` | protocol 判定、endpoint 构造、模型映射、auth strategy、prompt cache allowlist、xAI namespace flatten | `crates/router` + `crates/protocol-bridge` | 抽出纯函数；删掉 CC Switch UI/provider DB 假设 |
| `proxy/handlers.rs` | transformed/pass-through 分派，compact、alpha search、images，错误响应 | `crates/router/src/handlers/*` | 复用 endpoint/response 语义；重接本项目 route decision |
| `proxy/forwarder.rs` | 官方 OAuth 透传校验、头过滤、重试、URL rewrite、响应处理 | `crates/router/src/upstream/*` | 只移植 Codex 相关最小闭包，建立独立官方与自定义 header policy |
| `proxy/providers/reasoning_bridge.rs` | 处理不可直接公开的 opaque/signed reasoning | 第二阶段 | 只有目标 provider 需要时启用，必须验证不可跨上游复用 |

对应源码：

- [Codex 配置与官方 OAuth 所有权](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/codex_config.rs#L3465-L3557)
- [Codex provider 判定、映射和 auth](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/codex.rs)
- [Responses↔Chat 完整转换](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/transform_codex_chat.rs)
- [Chat 流式状态机](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/streaming_codex_chat.rs)
- [Responses SSE 构造](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/codex_responses_sse.rs)
- [Chat 历史补全](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/codex_chat_history.rs)
- [官方授权转发与 endpoint rewrite](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/forwarder.rs)
- [转换/透传/compact/search/images handler](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/handlers.rs#L912-L999)

这些文件构成的是超过八千行的协议状态机与测试闭包；现有 Router 的数百行近似转换不可能靠补两个字段达到同等兼容度。

#### 7.1.1 CC Switch 的请求转换流水线

CC Switch 并非看到 `/responses` 就机械改成 `/chat/completions`，而是先形成与本次请求绑定的上下文：

1. 根据显式 provider metadata、endpoint 和 base URL 判定 native Responses 还是 Chat；本项目只保留 metadata 优先原则，运行时禁止仅靠 URL 猜测。
2. 在模型映射前保存 inbound logical model，再按 provider catalog 映射 upstream model；这样日志、历史和响应能区分用户选择与实际发送值。
3. 从 tools 建立 `CodexToolContext`，处理 function、namespace、custom、tool_search、最大名称长度和名称碰撞，并保存可逆映射。
4. 按输入原顺序遍历 Responses items，把 instructions、messages、reasoning、function/custom call、call output 和媒体转换成 Chat 可表示的序列；不是分别收集后重新排序。
5. 对不可表示但可安全降级的字段走 provider profile；对会破坏工具闭环或安全域的字段返回 context-boundary，而不是静默删除。
6. 最后才应用 provider-specific reasoning、prompt cache 和请求头策略。

本项目应把这六步实现为纯函数 pipeline，并让 `RouteDecision` 决定 profile；handler 不得自行重复协议判断。

#### 7.1.2 工具调用为何需要可逆上下文

Responses 支持的不只有传统 function tool。namespace/custom/tool_search 进入 Chat 时经常需要 flatten 或编码为 function name；返回时还必须恢复原始类型、名称、call id 和 output 关联。CC Switch 为此维护请求级映射并检测碰撞、超长名和工具丢失。

本项目需要同时保存：

- `wire_name -> original kind/namespace/name`；
- Responses `call_id`、Chat tool call id 与输出 item 的关系；
- 每个并行调用的 argument accumulator；
- 被 provider 拒绝或截断的调用状态。

若流结束时存在发出过 delta 但无法构造完整调用的工具，整次响应必须 failed/incomplete；不能只把已生成文本返回 completed，否则 Codex 会把工具状态永久卡死。

#### 7.1.3 SSE 不是文本转发，而是状态机

CC Switch 的 streaming adapter 把 Chat chunk 喂入状态机，再由独立 Responses SSE builder 产生事件；二者分离使 ID、item index、content index、sequence 和 terminal event 可验证。它还处理 CRLF、多行 data、UTF-8 分片、usage-only chunk、并行工具 argument、reasoning 字段差异和 `[DONE]`。

本项目应保持同样的分层：

```text
byte stream -> SSE decoder -> ChatChunk -> semantic state
            -> ResponsesEvent builder -> encoded SSE byte stream
```

任何 parse 错误、上游非 2xx、连接中断和客户端取消都必须进入单一 terminal transition；禁止从多个代码分支各自拼 JSON 字符串。

#### 7.1.4 历史补全的真正作用

CC Switch 的 history 不是聊天记录 UI，而是协议转换所需的 state cache：当 Codex 只提供 `previous_response_id + function_call_output` 时，Chat 上游需要看到对应 assistant tool call 才能接受 tool result。它会记录此前 Responses 输出并在下一请求中补齐。

OmniBridge 还多一个 CC Switch 默认路径不必完整处理的问题：用户会跨 official/custom route 切换。因此 ledger 既要补工具前史，也要执行安全域转换；“同 route 透传私有 ID”和“跨 route 只水化 portable envelope”必须是两个分支。

#### 7.1.5 官方订阅透传的核心不是复制一个 HeaderMap

CC Switch 会确认请求使用的是 Codex 官方授权语义、核对账户绑定，再允许 inbound `Authorization` 到官方 upstream；对于第三方 route，它会删除 authorization/x-api-key 后按目标策略注入。它还单独处理 `ChatGPT-Account-Id` 与 `x-codex-*` 等 Codex 上下文头。

本项目应把 header policy 写成可审计表，而不是“转发除 hop-by-hop 外的全部头”：

```text
OfficialPolicy = explicit allowlist + account validation + no failover
CustomPolicy   = strip official/session/capability + inject selected auth + provider allowlist
ResponsePolicy = safe response headers only + generated request/route ids
```

此处的测试必须使用 canary token：给每个安全域放一个唯一假值，再断言其他 upstream 永远收不到它。

### 7.2 CC Switch 中不能照搬的部分

| 部分 | 不照搬原因 | 本项目做法 |
|---|---|---|
| `provider_router.rs` 的当前 provider/failover | 它按全局当前 provider 选路，不是按 Codex 请求中的模型稳定选路 | 使用 exact logical model route；route generation 固定到一次请求和历史记录 |
| “统一 session bucket”的 `custom` provider id | 它统一的是 Codex 历史 bucket，不等于同一请求 provider 下的逐模型路由 | 统一 provider 由 stock Codex 配置，实际 provider 在 OmniBridge 内部决定 |
| UI、SQLite provider 管理和 Tauri command | 与 Linux CLI/远端 daemon 生命周期不匹配 | 保留本项目 registry/CLI，但升级 schema 和事务 |
| 对所有 provider 的自动 failover | 官方 OAuth 不能回退到第三方，工具调用中的隐式 failover 也会破坏历史 | 默认无 failover；只有同一安全域、同一协议合同下显式配置才允许 |
| 仅内存、固定 512 条 history | 服务重启和多 thread/多账户时不足 | 分区 LRU + TTL + 0600 SQLite 可选持久化 |
| 通过 URL/配置启发式猜协议 | 容易把兼容网关误判成 Chat | 导入时可建议，保存和运行时必须是显式 protocol/profile |
| 对 `/responses/compact` 只做 endpoint rewrite | Chat-only 上游未必支持 compact 语义 | official/native Responses 可透传；Chat-only 要么实现经过测试的本地策略，要么在 catalog/profile 中禁用远端 compact 并提供客户端 fallback |

### 7.3 移植方式要求

1. 先把 CC Switch 固定 commit 作为只读参考，不追随其 main 漂移。
2. 按上表建立独立 `protocol-bridge` crate，先原样带入最小依赖闭包和测试，再做本项目命名重构。
3. 每次结构性改动都保留 provenance 注释；`THIRD_PARTY_NOTICES.md` 列出复制文件和主要修改。
4. 用 golden fixtures 同时跑 CC Switch 原逻辑与本项目 port，比较结构化输出，避免“看起来一样”。
5. 只在通过 parity tests 后删除当前 `compatibility.rs` 和 Router 内嵌的临时 SSE 状态机。
6. 不复制 CC Switch 的真实用户配置、缓存、数据库或 secrets；只复制源码算法与公开测试夹具。

## 8. 协议兼容矩阵

### 8.1 请求侧

| Codex Responses 输入 | Native Responses 上游 | Chat Completions 上游 | 失败策略 |
|---|---|---|---|
| `instructions` | 按 dialect 保留 | 转首个 system/developer message | 目标不支持相应 role 时显式配置 fallback |
| string `input` | 保留 | 转 user message | 无损 |
| message items / content parts | 正规化后保留 | 转 messages/content；图像按 provider profile | 不支持的媒体 fail closed |
| function tool | 保留 | 转 Chat function tool | 名称冲突/超长按稳定映射表处理 |
| namespace/custom/tool_search | 按上游能力保留或 flatten | 用 CC Switch 编码算法转 function tools | 无法恢复映射时拒绝，不丢弃 |
| function/custom call output | 保留并检查 call id | 转 tool message | 缺失 call 前史时尝试 ledger；仍缺失则拒绝 |
| reasoning config | 方言映射 | 映射 `reasoning_effort`/provider 字段 | 未证明支持则剥离并在 capability 中关闭 |
| encrypted/opaque reasoning item | 仅同后端同安全域透传 | 通常不可表示 | 跨路由永不透传 |
| `previous_response_id` | 同 route/account/generation 可透传 | 用 ledger 展开历史 | 找不到记录时返回明确 resume boundary |
| prompt cache/safety id | 仅 allowlist provider 保留 | provider profile 映射 | 不跨 provider 复用 |
| token limit | dialect 映射 `max_output_tokens` 等 | 映射 `max_tokens`/`max_completion_tokens` | 越界或歧义时报错 |
| `stream` | 保留 | 保留，响应端转换 | provider 不支持流式时目录必须反映 |

### 8.2 响应侧

Chat 非流式响应必须转换为真正的 Responses 对象，至少包含稳定 `id`、`object`/type 语义、status、output items、content parts、tool calls、usage 和错误映射。不能像当前代码一样直接把上游 JSON body 原样返回。

Chat 流式响应必须维护以下状态，而不是只拼 `choices[0].delta.content`：

```text
response.created
  -> response.in_progress（若目标 Codex 期望）
  -> output_item.added
       -> content_part.added
       -> output_text.delta / reasoning_summary_text.delta / function_call_arguments.delta
       -> content_part.done
  -> output_item.done
  -> response.completed | response.failed | response.incomplete
```

必须覆盖：

- 多 choice 的明确策略（通常只允许/选择 index 0，并验证协议）；
- 多个并行 tool calls 与交错 argument delta；
- finish reason 到 completed/incomplete/failed 的映射；
- usage 到 input/output/reasoning/cached token 的可得映射；
- 首 chunk 没有 id、role-only chunk、空 delta、`[DONE]` 前后异常；
- CRLF、多行 `data:`、注释行、拆分 UTF-8 字符、超长行；
- 客户端取消时立即取消 upstream，不在后台继续计费；
- dropped/invalid tool call 时整体 fail closed，不能把纯文本伪装成成功。

### 8.3 端点矩阵

| 入口 | Official | Native Responses custom | Chat-only custom |
|---|---|---|---|
| `/v1/responses` | 透传至 ChatGPT Codex backend | 正规化后透传 | 完整双向转换 |
| `/v1/responses/compact` | 透传 | 上游证明支持时透传 | P0 阶段明确拒绝并触发可诊断 fallback；不得改名后盲发 Chat |
| `/v1/alpha/search` | 语义透传 | provider 明确支持时透传 | 不做 Responses→Chat 转换 |
| `/v1/images/generations` | 语义透传 | provider 明确支持时透传 | 独立 endpoint profile，不能假定 Chat endpoint 支持 |
| `/v1/images/edits` | 同上 | 同上 | 同上 |
| `/v1/models` | 返回已安装的逻辑模型视图 | 同一视图 | 同一视图 |
| `/v1/chat/completions` | 非 Codex 主路径 | 可作为兼容/测试入口 | 原生或适配；不能影响 Codex 的 Responses 主合同 |

## 9. 跨模型历史与切换算法

### 9.1 为什么当前“删除 `previous_response_id`”一定会坏

Codex 经常只发送本轮新增 input 并用 `previous_response_id` 指向服务端保存的前一响应。当前代码直接删除该字段，却没有把前史补回 input；结果是自定义上游只看到最后一个工具结果或最后一条用户消息，既不能继续工具循环，也不能维持上下文。

### 9.2 Ledger 最小记录

```text
response_id
thread/session fingerprint（若请求可得）
official account fingerprint（只存 hash，不存 account id 原文）
route_id + route_generation
protocol + upstream origin fingerprint
request model / upstream model
ordered normalized input items
ordered normalized output items
tool call id mapping
created_at / expires_at / terminal status
```

### 9.3 续接规则

| 前一响应与当前请求关系 | 行为 |
|---|---|
| 同 route、同账户、同协议、同 upstream 支持 | 透传 upstream `previous_response_id` |
| 同逻辑 route 但上游不支持 stateful Responses | 从 ledger 重建显式 history |
| official → custom | 只水化可移植的 message/tool output；删除官方私有 id、encrypted reasoning、cache/safety id |
| custom → official | 同样水化可移植内容；绝不把第三方 response id/签名 reasoning 送官方 |
| custom A → custom B | 视为跨安全域；只迁移 portable envelope |
| ledger miss / expired | 返回可识别的 `context_boundary`，提示 fork/new thread 或由上层重发完整历史；禁止静默失忆 |

### 9.4 重启与远端要求

P0 可先使用严格分区的内存 LRU，但这只能证明单进程会话；要达到 Remote Control 可用，P1 必须有可选 0600 SQLite/WAL 持久化，支持 Router 重启后的未完成工具循环恢复，并提供 TTL、最大字节数、清除命令和 schema migration。

## 10. 已确认问题总表

以下是截至本次源码、契约和本地检查可确认的问题全集；真实 provider E2E 仍可能暴露新的 provider-specific quirk，必须通过测试矩阵继续登记，不能声称此表证明所有互联网实现都兼容。

### 10.1 P0：不修就无法实现目标

| ID | 问题 | 证据/影响 | 修复 |
|---|---|---|---|
| P0-01 | Codex patch 固定旧 commit | 构建脚本拒绝其他 HEAD；对最新 Codex dry-run 已失败 | 默认删除 patched Core 依赖，改 stock provider+gateway；补丁只留实验模式 |
| P0-02 | 依赖已经移除的 Chat wire API 思路 | 最新 `WireApi` 只有 Responses | Codex 入口统一 Responses，Chat 仅在网关内部适配 |
| P0-03 | 自定义 catalog 缺 `shell_type` | 最新 `ModelInfo` 严格字段；当前 `custom_entry` 未写 | 按目标 Codex profile 生成，并用真实 Codex 反序列化验证 |
| P0-04 | catalog 没有 `model_messages` | custom model 得不到正确 base instructions | 从兼容模板继承/生成完整 model messages；做快照测试 |
| P0-05 | 只写 `model_catalog_json`，没有 provider | stock Codex 看见模型不等于请求会进 Router | 原子安装 `model_provider`、provider table、catalog 三者 |
| P0-06 | Router 对官方模型返回 501 | `!logical_model_id.contains('/')` 分支拒绝官方 | 增加官方订阅 route 和严格 OAuth passthrough |
| P0-07 | 用斜杠判断 official/custom | 模型命名规则不是授权边界，存在误路由 | registry 中显式 `route_class`，完整 ID 精确匹配，冲突时 sync 失败 |
| P0-08 | patched Core 只在局部分支分流 | stream/client 生命周期、compact、WS 等路径不一致 | 统一 stock provider base URL，所有相关 endpoint 由 gateway 承接 |
| P0-09 | Router 能力 token 占用 Authorization | 与官方 OAuth bearer 互斥 | 改独立 `x-codex-omnibridge-token`，常量时间比较，永不转发 |
| P0-10 | Router 默认把所有第三方密钥当 Bearer | Anthropic/x-api-key/自定义网关不能正确认证 | 显式 auth strategy + header allow/deny policy |
| P0-11 | Responses→Chat 只接受简单 message | 正常 Codex 工具/history item 会 context boundary | 移植 CC Switch 完整转换闭包 |
| P0-12 | Chat→Responses 流式状态机不完整 | 固定 ID、只读第一个 text/reasoning、无 tool/usage/error/index | 移植 SSE builder 和 streaming state machine |
| P0-13 | Chat 非流式响应未转换 | 当前直接 passthrough 上游 body，Codex 收到错误 schema | 实现并测试完整 non-stream conversion |
| P0-14 | 删除 `previous_response_id` 却不补历史 | 上下文和工具循环断裂 | route-aware history ledger + hydrate 算法 |
| P0-15 | 缺 `/responses/compact` | 长会话或 Codex 远端 compact 会失败；旧 patch 还主动拒绝 custom compact | official/native pass-through；Chat-only 明确 fallback/策略 |
| P0-16 | 缺 search/images 独立 endpoint | 官方/支持这些能力的模型在 agent 流中失败 | 实现 endpoint policy 与 semantic passthrough |
| P0-17 | provider 在 thread 固定 | 用两个 Codex provider 无法每轮切换 | 所有可切换模型共用 `omnibridge` provider |
| P0-18 | Remote Control 方向错误 | 本地 Desktop adapter 不决定远端 model/list 和请求 | 在远端安装 provider/catalog/service；本地保持 stock Desktop |
| P0-19 | Router 是 wrapper 子进程 | CLI/manager 退出即被 Drop 杀死，remote app-server 随后无后端 | systemd user 常驻服务，稳定端口/endpoint，启动依赖和 readiness |
| P0-20 | `--registry` sync 污染全局路径 | `IntegrationPaths::default()` 不从传入 registry 派生 | 所有路径从显式 context 构建；加入隔离测试 |
| P0-21 | keyring 失败静默写明文 `.credentials` | 用户未同意即可持久化秘密，且读取优先于 keyring | 默认 fail closed；显式 `--secret-backend=file` 才允许 0600 加密/明文策略 |
| P0-22 | 现有测试制造假阳性 | 测试甚至断言 custom 没有 `shell_type`，真实 catalog test 可静默 skip | 对目标 Codex binary 做非可选 contract test；缺 fixture 即失败 |
| P0-23 | 旧 thread 的 provider 不是 `omnibridge` | 仅换 model 无法改变 provider | 提供显式 resume/fork migration；不静默改历史 |
| P0-24 | 官方与自定义头没有安全域隔离 | 当前仅转 Accept，未来直接透传会泄漏 OAuth | 两套独立 header policy；官方 auth validation；custom 先清洗后注入 |

### 10.2 P1：达到可靠日常使用前必须修

| ID | 问题 | 影响 | 修复 |
|---|---|---|---|
| P1-01 | catalog schema 由手写字段追版本 | Codex 更新即再次损坏 | compatibility profile + schema fingerprint + target-binary validation |
| P1-02 | capability 只是 reasoning/tools/images/files/streaming 布尔值 | 无法描述 tool 类型、reasoning 方言、compact/search/images | 引入 ProvenCapabilities 与 route/tool/dialect profile |
| P1-03 | protocol 只分 Responses/Chat | 不能表达兼容网关、Anthropic、xAI quirks | 扩展显式 adapter/dialect，不再靠 URL 猜测 |
| P1-04 | 请求头只转发 Accept | trace、account、content negotiation 与上游错误语义丢失 | endpoint/route 级 allowlist，hop-by-hop denylist |
| P1-05 | 不处理压缩请求体 | 新 Codex/代理使用压缩时 parse 失败 | 有限 gzip/br 解码，防 zip bomb |
| P1-06 | URL join 仅字符串拼接 | 带 prefix、已有 `/v1`、完整 endpoint、query 时重复/丢路径 | 使用 URL 类型和 endpoint rewrite 规则，建立矩阵测试 |
| P1-07 | base URL 校验未拒绝 userinfo/query/fragment | 凭据混入 URL、路由歧义 | 保存时正规化并拒绝这些分量；仅 HTTPS 或 loopback HTTP |
| P1-08 | 无可靠 timeout/retry/cancel | 卡死、重复计费、客户端取消后继续跑 | 分阶段 timeout；只在安全阶段重试；传播 cancellation |
| P1-09 | 无 backpressure/流大小治理 | 慢客户端导致内存/连接耗尽 | bounded channels、stream idle timeout、per-route concurrency |
| P1-10 | reasoning dialect 不完整 | 推理级别无效或签名 reasoning 泄漏/报错 | 移植 common/reasoning bridge；安全域约束 |
| P1-11 | tool 名称/namespace/custom tool 不完整 | MCP、tool_search、custom tools 不能正常循环 | 移植 CC Switch tool context 和稳定映射表 |
| P1-12 | history 无 thread/account 隔离 | response id 碰撞或跨账户串历史 | 复合 key、账户 hash、route generation、TTL |
| P1-13 | history 仅内存会在服务重启丢失 | 远端长会话中断 | 可选 SQLite/WAL、恢复测试、清除机制 |
| P1-14 | config 用逐行 TOML 编辑 | 注释、多行/inline table、重复 key 易损坏 | 使用 semantic TOML editor；完整 parse 后 transaction commit |
| P1-15 | fixed `.tmp` 且无文件锁/CAS | 并发 sync、崩溃、symlink 会覆盖错误对象 | unique temp、fsync file+dir、advisory lock、expected hash |
| P1-16 | integration manifest 只管理一个字段 | 无法原子回滚 provider/catalog/服务配置 | 多资源 transaction manifest + 原始 snapshot hashes |
| P1-17 | endpoint 健康检查只看通用 `/healthz` | 可能复用另一 registry/旧 generation 的 Router | readiness 返回 instance/registry/config hashes 并核对 |
| P1-18 | 子进程 stdout/stderr 被丢弃 | 远端失败无证据 | journald + request id + doctor tail，默认脱敏 |
| P1-19 | headless secret backend 未定义 | SSH/systemd 无 Secret Service 时行为随机 | 安装时选择 keyring、systemd credential、env/file；doctor 验证可读性 |
| P1-20 | 官方失败可能被误当 provider 故障 | 若实现自动 failover 会把官方上下文送第三方 | official route 永不 failover，custom 也默认关闭 |
| P1-21 | provider reload 非原子 | 一半请求用旧 catalog、一半用新 auth/profile | immutable generation，验证成功后单指针 swap |
| P1-22 | 模型选择与 tool catalog 解耦 | UI 允许工具但上游无法执行 | 由同一 route profile 生成 catalog tool capabilities |
| P1-23 | 缺真实升级/降级策略 | 新 Codex schema 变化后远端突然不可用 | version probe、known-good profile、preflight、原子 rollback |
| P1-24 | compact 对 Chat-only 无合同 | 长会话可能突然在阈值处失败 | 明确本地 compact 或禁用/客户端 fallback，做长上下文 E2E |

### 10.3 P2：可维护性与发布质量

| ID | 问题 | 修复方向 |
|---|---|---|
| P2-01 | CLI 和 manager 重复 provider fetch/路由逻辑 | 收敛为一个 application service |
| P2-02 | `status` 只打印元数据 | 改为 doctor：配置、权限、schema、service、catalog、OAuth presence（不显示内容）、route smoke |
| P2-03 | Desktop/CLI binary 版本可能报告 `0.0.0` | 用 build metadata、commit、protocol profile 三元组判定兼容，不信单一版本字符串 |
| P2-04 | 本地 Desktop adapter 侵入用户 standalone runtime | Remote 模式弃用；仅实验 Core-patch 模式保留且显式 opt-in |
| P2-05 | 日志缺 route/阶段指标 | 结构化记录 route id、协议、状态、TTFB、tokens、转换错误，不记录内容 |
| P2-06 | 无性能/负载基线 | 加并发、长 SSE、慢消费者、大工具参数 soak tests |
| P2-07 | 无正式服务安装/卸载合同 | systemd user unit、enable/disable、rollback、保留用户 registry/secrets 的卸载选项 |
| P2-08 | 缺 third-party provenance | 增加 notices、固定 commit、更新流程和差异审计 |
| P2-09 | provider quirk 无版本管理 | profile id/version 写入 registry 与日志，迁移显式化 |
| P2-10 | 缺可复现远端验收包 | 提供 fixture provider、probe client、Remote Control acceptance script 和证据模板 |

## 11. 配置、目录与事务重建

### 11.1 Compatibility profile

每个受支持 Codex runtime 都要有 profile：

```text
profile_id
codex commit/version range
ModelInfo schema fingerprint
required/optional fields
safe official template slug or embedded template hash
provider config fields
expected app-server initialize/model-list behavior
compact/search/images feature flags
```

目录生成步骤：

1. 对目标 binary 运行只读 version/build probe；
2. 运行 `codex debug models --bundled` 并严格解析；
3. 匹配 profile，未知 schema 立即停止，不写配置；
4. 选择一个已审核的模板，保留 shell/tool/model messages 等运行合同；
5. 仅覆盖 logical slug、display、description、reasoning levels、context 和已经证明的 capability；
6. 把目录交给目标 Codex 的真实类型/app-server `model/list` 验证；
7. 只有全部通过才进入事务提交。

不要再把 `_template` 参数忽略后手写几十个字段；也不要盲目 clone 最新官方模型并继承第三方不支持的 hosted tools。

### 11.2 原子安装事务

事务对象至少覆盖：

- Codex `config.toml` 中 `model_provider`、`model_catalog_json`、`model_providers.omnibridge`；
- generated catalog；
- Router registry generation；
- local capability；
- systemd user unit 与环境/credential 引用；
- manifest 和原始文件 hash。

执行顺序：

```text
lock -> read/validate -> stage unique files -> start candidate Router
-> readiness with expected hashes -> target Codex preflight/model-list
-> commit catalog/config -> restart/reload app-server when required
-> postflight -> persist manifest -> unlock
```

任何一步失败都恢复原始 bytes 和服务状态；如果原始目标在事务中被外部修改，停止并报告冲突，不覆盖。

### 11.3 卸载

卸载默认只删除本项目明确拥有且 hash 未变化的 provider table、catalog、service 和 capability；恢复原 model provider/catalog 配置。registry、第三方 secret、history ledger 是否删除必须由用户单独选择。绝不删除 `auth.json`、官方历史或整个 `~/.codex`。

## 12. Linux 常驻服务与 Remote Control

### 12.1 服务合同

Router 应作为 systemd user service 安装：

- `After=network-online.target`，但 readiness 不等同于网络已连通；
- 固定 loopback 端口，或由安装事务固定端口后写入 provider 配置；
- `Restart=on-failure`，有限重启退避；
- `UMask=0077`；
- 禁止监听公网；
- 日志进入 journald 且默认脱敏；
- `/healthz` 只表示进程活着，`/readyz` 返回非敏感的 instance、registry generation、catalog hash、build/profile；
- reload 先验证完整 registry，再原子切 generation。

### 12.2 远端安装流程

1. 在远端安装 CLI 与 Router service。
2. 在远端登录官方 Codex；OmniBridge 只检查登录存在性，不读取/输出 token。
3. 在远端添加自定义 provider secret 和模型。
4. 执行 `codex-mp sync --target-codex <remote stock binary>`，完成 target schema preflight。
5. 启动并验证 `codex-mp-router.service`。
6. 先用远端本地 CLI 分别完成 official/custom/official 三轮与工具循环。
7. 再启动远端 `codex remote-control` 并从本地 Desktop 配对。
8. 本地 Desktop 调用远端 `model/list`，确认官方与自定义模型出现。
9. 在同一远端 thread 做 official→custom→official，核对远端 route audit、历史和工具结果。

### 12.3 旧 thread 迁移

- provider 为 `openai` 的旧 thread 不会因为 catalog 改了就变成 `omnibridge`；
- CLI 提供 `resume --through-omnibridge` 或清晰指导，通过 app-server 的 thread resume provider override 建立受控迁移；
- 若无法保证历史完整，默认 fork 为新 thread，并把“从这里开始通过 OmniBridge”作为显式边界；
- 不允许在磁盘上批量篡改官方 rollout/history 文件。

## 13. 分阶段实施计划

### Phase 0：冻结契约和导入来源

交付：

- 新增 `THIRD_PARTY_NOTICES.md`；
- 固定 CC Switch commit 与 OpenAI Codex compatibility profile；
- 保存合法的 Responses/Chat request、SSE、tool、reasoning、history golden fixtures；
- 写出 security/header policy 的单元测试，测试先失败。

退出条件：每个 P0 问题都有对应自动化测试或明确的真实 E2E 用例。

### Phase 1：协议桥移植

建议新增：

```text
crates/protocol-bridge/
  src/chat/request.rs
  src/chat/non_stream.rs
  src/chat/stream.rs
  src/responses_sse.rs
  src/tool_context.rs
  src/reasoning.rs
  src/history.rs
```

交付：完整 Responses↔Chat、SSE、工具、reasoning、ledger 内存版；用 CC Switch parity fixtures 验证。此阶段不得连接真实官方账号。

退出条件：转换测试覆盖本文件 8.1/8.2 所有行，当前临时 adapter 尚可保留但不再被新 Router 使用。

### Phase 2：统一网关与安全边界

修改：

- `crates/core`：RouteProfile、AuthStrategy、EndpointPolicy、registry generation；
- `crates/router`：独立 capability、official/custom 两套 header policy、endpoint handlers、timeout/cancel/retry；
- `crates/credentials`：禁止隐式明文 fallback；
- `crates/manager`：从 wrapper ownership 改为 service control/readiness。

退出条件：mock official/custom upstream 下，official→custom→official 同 thread、工具循环、取消、401/429/5xx 和泄漏测试全部通过；抓包断言 custom 永远看不到官方头。

### Phase 3：目录与 stock Codex 集成

修改：

- `crates/catalog`：compatibility profiles 和真实 target validation；
- `crates/integration`：semantic TOML、多资源事务、锁/CAS/rollback；
- `crates/cli`：`doctor`、`sync --target-codex`、服务安装与迁移命令；
- 默认构建不再依赖 `patches/codex/*` 和 patched Codex。

退出条件：最新 stock CLI/app-server 可以 initialize、model/list、创建 thread 并分别调用 mock official/custom；`--registry` 隔离测试通过。

### Phase 4：真实本地 E2E

按以下顺序，不得合并结果：

1. 官方订阅纯文本；
2. 官方工具调用；
3. 自定义 native Responses 纯文本/工具/流；
4. 自定义 Chat 纯文本/工具/流；
5. 同 thread official→custom→official；
6. 模型切换发生在未完成工具循环前后的边界；
7. compact 和服务重启恢复；
8. 凭据/头泄漏审计。

退出条件：每项有 request id、route id、状态和人工可见结果，秘密均已脱敏。

### Phase 5：Remote Control E2E 与发布

交付 systemd user 安装/卸载、远端 doctor 和可复现验收记录。用本地 stock Desktop 连接远端，完成模型列表、调用、切换、工具、断线重连和 Router 重启恢复。

退出条件：下节所有 Required Remote 项目 PASS；否则版本不得标为“支持 Remote Desktop”。

### Phase 6：清理旧机制

在新路径完成上述验收后：

- 删除 Router 中内嵌简化 SSE 和旧 compatibility 代码；
- 把 patched Core/Desktop adapter 标记 experimental/deprecated，或完全移到单独 feature/repository；
- 更新 README，不再用“单元测试通过”暗示官方订阅或 Remote Control 已证明；
- 保留旧 manifest 的安全卸载/恢复能力。

## 14. 验收矩阵

### 14.1 自动化协议测试

| 维度 | 必测值 |
|---|---|
| 上游 | official mock、native Responses、Chat、malformed server |
| 返回 | stream、non-stream、early error、mid-stream error、disconnect |
| input | text、multi-part、image、function、custom、namespace、tool_search、tool output |
| reasoning | none、summary、provider dialect、opaque/signed |
| history | no previous、same route、cross route、ledger miss、expired、account mismatch |
| SSE | LF/CRLF、多行 data、UTF-8 split、empty delta、parallel tools、usage、DONE missing |
| auth | missing/wrong capability、official OAuth、wrong account、自定义 Bearer/x-api-key/custom header |
| lifecycle | reload、restart、concurrent sync、cancel、timeout、slow consumer |

### 14.2 Required 本地产品 E2E

| 用例 | 成功标准 |
|---|---|
| stock Codex model/list | 官方和 custom 均显示，能力与 route profile 一致 |
| official call | 使用现有订阅成功；`auth.json` hash 不变 |
| custom Responses | 文本、工具、reasoning、usage、stream 均可用 |
| custom Chat | 同上，返回给 Codex 的始终是合法 Responses |
| O→C→O | 同一 thread 连续三轮，上下文可解释地保留，无私有 ID/密钥跨域 |
| compact | official/native 成功；Chat-only 行为符合明确合同 |
| restart | Router 重启后按承诺恢复或给出明确边界，不静默丢历史 |
| secret audit | 日志、process args、manifest、catalog 无上游 key/OAuth |

### 14.3 Required Remote Control E2E

| 用例 | 成功标准 |
|---|---|
| 远端 catalog | 本地 Desktop 显示远端 custom，而本地无需安装相同 registry |
| 远端 official | 请求由远端发出并使用远端官方登录 |
| 远端 custom | 请求由远端 Router 发出并使用远端 secret |
| 同 thread 切换 | Desktop 选择模型后远端 route 与选择一致，历史不串域 |
| tool loop | shell/MCP/function 完成至少两轮调用与 output 回填 |
| 断线重连 | Desktop 重连后 thread 可 resume，provider 不意外回到 `openai` |
| service restart | Router 重启和 app-server 重连行为可预测、可诊断 |
| 本地隔离 | 本地 Desktop runtime、auth、catalog 无需被替换或写入 |

### 14.4 汇报规范

发布或检查报告必须逐项使用：

- `PASS`：有本轮直接证据；
- `FAIL`：已执行且不满足；
- `NOT RUN`：未执行；
- `NOT PROVEN`：执行了邻近检查，但不足以证明该合同。

构建 PASS 不能替代真实 OAuth，mock provider PASS 不能替代 Remote Control，Remote model/list PASS 不能替代实际调用。

## 15. 明确不采用的方案

### 15.1 继续以旧 Core patch 为主线

拒绝原因：紧耦合单个上游 commit；本次已经对最新源码 apply 失败；每次 Codex client、compact、WS、app-server 更新都要重新审计所有发送路径。它只适合作为“官方 OAuth 永不进入 Router”这一更强隔离要求下的高级模式。

### 15.2 官方走内建 `openai`，custom 走独立 provider

拒绝原因：provider 在 thread 级固定，Desktop 每轮模型选择不能切 provider。可以开新 thread，但不满足用户要求的自由切换。

### 15.3 只用 `openai_base_url` 重定向内建 provider

这是较少配置的候选，但内建 OpenAI provider 的 WebSocket 能力和其他名称/后端特例不可完全覆盖，网关若只实现 SSE 会产生先失败再降级的路径；独立 `omnibridge` provider 能显式关闭 WebSocket并管理 catalog，故优先采用。

### 15.4 继续补现有简化 Chat bridge

拒绝原因：Codex agent 协议的复杂度主要在工具、流式状态、历史和 reasoning，不是字段重命名；CC Switch 已有经过大量测试的完整实现，继续重写会重复造轮子并持续制造边界错误。

## 16. 开发过程中的禁止项

- 不读取、打印、复制或提交用户 `auth.json` 内容；
- 不把官方 OAuth 存入本项目 registry、SQLite、keyring 或日志；
- 不在没有用户明确选择时把第三方 secret 从 keyring 降级到文件；
- 不重置、清理或覆盖现有 dirty worktree；
- 不为了通过测试降低 catalog/tool 能力声明与 Router 实际能力之间的一致性；
- 不用模型名是否包含 `/` 决定安全边界；
- 不让 official route 参与自动 failover；
- 不把“能显示模型”“HTTP 200”“打包成功”当作 agent 可用；
- 不在 Remote Control 验收前宣称 Desktop 支持完成；
- 不在未保留 MIT notice/provenance 时复制 CC Switch 代码。

## 17. 给实现模型的完成定义

实现不是“写完 P0 代码”即完成，而是：

1. 默认路径已使用 stock Codex；
2. 所有 P0 已修，P1 中与安全、协议、Remote Control 有关的项目已修；
3. CC Switch 协议闭包和测试按固定 commit 带 attribution 移植；
4. 官方 OAuth、自定义 secret、历史安全域和本地 capability 的头泄漏测试通过；
5. 最新目标 Codex 的 catalog/app-server contract test 通过；
6. 本地 official→custom→official 与 Remote Desktop→remote official/custom E2E 分别给出证据；
7. 没有执行的真实环境验证明确标为 NOT RUN/NOT PROVEN；
8. 文档、安装、rollback、uninstall 和 notices 同步更新。

## 18. 一句话实施提示词

> 请严格以 `docs/CCSWITCH_MECHANISM_AUDIT_AND_REBUILD_PLAN.md` 为唯一机制合同，在保留现有 dirty worktree 和官方 `auth.json` 所有权的前提下完成 stock Codex 单一 OmniBridge provider、远端常驻路由、版本化 catalog、官方 OAuth 安全透传、第三方凭据隔离及完整 Responses↔Chat/工具/reasoning/SSE/history/compact 兼容，按文档固定 commit 直接移植 CC Switch 的相关算法与测试并补齐 MIT notices，连续实现 P0 和必要 P1、执行本地 official→custom→official 及 Remote Control E2E，并逐项以 PASS/FAIL/NOT RUN/NOT PROVEN 和可复核证据交付而不要停在分析或计划。

## 19. Sources

### OpenAI 官方资料

- [Responses API：Create a model response](https://developers.openai.com/api/reference/cli/resources/responses/methods/create)
- [Chat Completions API](https://developers.openai.com/api/reference/cli/resources/chat)
- [Codex provider contract at audited commit](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/model-provider-info/src/lib.rs#L61-L155)
- [Codex model catalog contract at audited commit](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/protocol/src/openai_models.rs#L402-L507)
- [Codex turn/start contract](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/app-server-protocol/src/protocol/v2/turn.rs#L166-L229)
- [Codex thread/start contract](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/app-server-protocol/src/protocol/v2/thread.rs#L62-L71)
- [Codex app-server model/list implementation](https://github.com/openai/codex/blob/ee6814bfa4889fe9b2b3dcc9cc8bdd91effa8ab8/codex-rs/app-server/src/request_processors/catalog_processor.rs#L247-L294)

### CC Switch 一手源码

- [CC Switch repository](https://github.com/farion1231/cc-switch/tree/1d5d90f4aba88447d422a16cdec5282ec5331fd7)
- [Codex config and OAuth ownership](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/codex_config.rs#L3465-L3557)
- [Codex provider adapter](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/codex.rs)
- [Responses↔Chat conversion](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/transform_codex_chat.rs)
- [Chat streaming conversion](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/streaming_codex_chat.rs)
- [Responses SSE builder](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/codex_responses_sse.rs)
- [Chat history ledger](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/providers/codex_chat_history.rs)
- [Codex handlers and standalone endpoints](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/handlers.rs#L912-L999)
- [Forwarding, header policy and official auth validation](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/src-tauri/src/proxy/forwarder.rs)
- [CC Switch MIT License](https://github.com/farion1231/cc-switch/blob/1d5d90f4aba88447d422a16cdec5282ec5331fd7/LICENSE)
