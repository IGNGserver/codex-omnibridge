# Codex 集成说明

> 规范说明：本文件保留为历史集成记录，不是当前机制合同。当前实现、验收和安全边界一律以
> [`CCSWITCH_MECHANISM_AUDIT_AND_REBUILD_PLAN.md`](./CCSWITCH_MECHANISM_AUDIT_AND_REBUILD_PLAN.md)
> 为准；本文件中的旧 patched/openai 路径仅代表历史状态或 legacy/experimental 资料。

## 结论先行

本项目的集成分成两个层次：

1. **Catalog 层**：把 namespaced custom model 加入当前安装版 Codex 的 `model_catalog_json`，让原生 Model Picker / app-server `model/list` 能看到它。
2. **默认数据面层**：把 stock Codex 配置到单一 `omnibridge` Responses provider，所有请求先进入 loopback Router，再由 Router 按 logical model 选择官方或第三方上游。这样同一 Thread 的 `turn/start.model` 可以跨官方和自定义模型切换。

默认 stock 路径已经通过隔离 fixture 的真实 stock `codex exec` custom turn，以及 stock `codex-app-server` 同一 Thread 的 Official → Custom → Official；旧的固定 Core patch 仍保留为 legacy/experimental 路径，并通过上游 `codex-core` 编译和 patched caller 验证。当前还提供 Linux、Windows、macOS Desktop 的可回滚 runtime adapter、跨平台 Router supervisor 和 Tauri 面板/托盘骨架。真实 ChatGPT Account、真实第三方 Provider、Desktop picker、Remote Control 和 Windows/macOS 原生 patched Codex artifact 仍未完成产品级验收。因此 P0 数据面在受控 fixture 下为 `PASS`，完整产品结论仍为 `NOT PROVEN`。

## 不变量

默认 stock 集成会原子更新以下语义配置：

- `model_provider = "omnibridge"` 及对应 provider table；
- `model_catalog_json` 生成的 catalog；

实现刻意不修改：

- `auth.json`、OAuth token 或 ChatGPT Account 状态；
- Thread provider、Thread ID 或 Thread 存储；
- 官方模型在 Router 中的原生官方 backend 路径；

API key 默认通过 `codex-mp-credentials` 的系统 keyring 实现读取。当前工作树还保留了 headless 的环境变量/文件 fallback；它不写入 Codex registry、catalog、manifest 或日志，但安全等级不能等同于系统 keyring，后续发布前需要单独验收。stock 与 legacy custom 请求都使用 Router capability token；ChatGPT OAuth 不作为第三方 Provider 的 Authorization 转发。

## Catalog 来源与合并

`codex-mp sync` 和 `codex-mp repair` 执行当前传入 binary 的：

```text
codex debug models --bundled
```

默认 binary 是 `codex`，可通过 `--codex-bin /absolute/path/to/codex` 覆盖。输出必须是根对象并包含 `models` 数组。合并器原样保留官方模型及顶层字段；自定义条目由显式安全字段重建，避免把官方 shell、hosted tool、账号指令或未来未知能力复制给第三方模型。

自定义模型使用 namespaced slug：

```text
newapi/qwen3.8
openrouter/deepseek-v4
local/qwen3.8
```

官方 slug 保持原样。Custom 条目只宣传显式 registry 能力；Codex-specific hosted tools、Web Search、Image Generation 等能力不能因为 Catalog 字段存在就视为已支持。

## patched Core 的真实调用路径

补丁绑定上游 commit：

```text
73a1148c9c775c2a4616ce5096291740a00ed68a
```

补丁新增 `core/src/logical_routing.rs`，并在 `core/src/client.rs` 做以下工作：

```text
ModelInfo.slug
    ├── 无 `/`：LogicalRoute::Official
    │       → 现有 OpenAI/ChatGPT Responses/WebSocket 路径
    └── 有 `/`：LogicalRoute::LocalRouter
            → 读取 CODEX_MP_ROUTER_ENDPOINT_FILE
            → POST loopback /v1/responses
            → x-codex-omnibridge-token: <raw Router capability token>
            → Router 按 registry 转到第三方 Provider
```

路由发生在真实 `ModelClientSession::stream()`，而不是在 Catalog、独立 GUI、DOM 或 shell wrapper 中。官方模型仍走原生 transport；Custom 模型没有被修改成新的 Codex Provider。

Custom 路径还具有这些明确边界：

- 不使用 Codex Responses WebSocket prewarm；
- 不发送官方 encrypted reasoning/include 字段；
- custom context compaction 明确返回错误，不静默送入官方 backend；
- 请求和 SSE 由现有 Codex Responses client 接收，Router 再按 Provider protocol 做请求转换；
- Router capability token 由 endpoint file 发现，Core 不从 `auth.json` 读取第三方 credential。

### 构建和应用补丁

```bash
patches/codex/73a1148c9c775c2a4616ce5096291740a00ed68a/apply.sh \
  /path/to/codex-rs

cd /path/to/codex-rs
CARGO_TARGET_DIR=/path/to/codex-rs-target \
OPENSSL_DIR=/usr \
OPENSSL_LIB_DIR=/usr/lib/x86_64-linux-gnu \
OPENSSL_INCLUDE_DIR=/usr/include \
cargo build -p codex-cli --release
```

Desktop 还必须构建同一 commit 的 `codex-app-server`；推荐直接运行
`scripts/build-patched-codex.sh`，它会同时输出 raw `codex-mp-codex-bin`、
`codex-mp-app-server-bin` 和 `codex-mp-build.json`。Desktop adapter 会拒绝缺少
或不匹配该 metadata 的 app-server，避免把未经验证的二进制放进 Desktop runtime。

`apply.sh` 只接受完全匹配的上游 HEAD，并使用 `git apply --check`；不会 reset、clean、覆盖用户修改或自动提交。补丁目录的 README 记录文件列表、SHA-256、构建约束和已知限制。

Windows 使用 `scripts/build-patched-codex.ps1`，由 PowerShell 调用相同的 pinned
commit、`git apply` 和 Cargo 构建流程；它输出 `.exe` 二进制和 `.cmd` 启动器。
macOS 使用 Unix 脚本，但生成的启动器只依赖 POSIX `sh`，不依赖 Bash 专有的
`/dev/tcp`。

### 启动顺序

可以手动先启动 Router：

```bash
ENDPOINT_FILE="$HOME/.config/codexmultiprovider/router-endpoint.json"
codex-mp router --port 0 --endpoint-file "$ENDPOINT_FILE"
```

再让 patched Codex CLI/app-server 继承同一 endpoint file：

```bash
export CODEX_MP_ROUTER_ENDPOINT_FILE="$ENDPOINT_FILE"
/path/to/patched/codex app-server
```

也可以直接使用跨平台启动器，由 Rust supervisor 负责复用或启动 Router：

    codex-mp launch --codex-binary /path/to/codex-mp-codex-bin -- app-server
    codex-mp launch --app-server-binary /path/to/codex-mp-app-server-bin

如果 Desktop/Bridge 不是从该 shell 继承环境，必须在其进程启动配置中设置同一变量。Core 会在 `ModelClient` 初始化时读取 endpoint；文件缺失或权限/URL/token 不合法时，Custom turn 明确失败，Official turn 不被改道。

## ChatGPT Desktop runtime adapter

`codex-mp desktop install` 先发现当前用户的 Desktop Codex runtime，并要求 Desktop
完全退出；之后将 patched app-server 与 Desktop 原有 `codex-code-mode-host` 复制到
独立 runtime，记录 SHA-256 和可回滚 manifest。Linux 的入口是
`CODEX_HOME/packages/standalone/current/bin/codex`，macOS 的入口是
`ChatGPT.app/Contents/Resources/codex`，Windows 的入口是 Desktop 在
`%LOCALAPPDATA%\OpenAI\Codex\bin` 中校验后的 `codex.exe`。

Linux 会在原入口位置写入 POSIX launcher。Windows/macOS 的官方 Desktop 代码提供
`CODEX_CLI_PATH` 覆盖入口，因此 adapter 将原生 `codex-mp` 复制到自己的
`desktop-launcher/<version>/` 目录，并将覆盖写入 Windows 当前用户环境注册表或
macOS 当前用户的 `launchctl` 环境；官方 Desktop runtime 不被覆盖。这一点很重要：
Windows 下直接改写缓存中的 `codex.exe` 会被 Desktop 下一次启动时的完整性复制恢复，
macOS 下直接改写 app bundle 会使官方签名失效。

入口在 manifest 写入前才会切换，catalog 同步失败会执行恢复。`desktop status` 会
同时验证官方入口、managed launcher、sidecar 配置、runtime、backup 和
`CODEX_CLI_PATH`；任何用户改动、Desktop 更新或环境覆盖漂移都会报告 `drifted`
并拒绝继续覆盖。`desktop restore` 和 `uninstall` 只清理本项目创建的 launcher、
runtime、backup、manifest 与环境覆盖，不触碰 OAuth、Thread/历史存储或官方模型
身份。

Windows/macOS 安装器分别为 `installer/install-windows.ps1` 和
`installer/install-macos.sh`。设置 `CODEX_MP_INSTALL_DESKTOP=1` 才会安装 Desktop
adapter；默认安装只复制 CLI 与 patched artifact。Windows 写入用户环境后还会发送
标准 environment-change 广播，macOS 使用 `launchctl setenv`；两者的 GUI Desktop
都必须在安装后完全退出并重新打开。Desktop 自动更新后必须重新执行 `desktop status`，
不能将旧 manifest 直接套到新 runtime。

## Router 安全边界

Router 默认只绑定 `127.0.0.1`，端口可以为 `0`。启动后写入 JSON endpoint file：

```json
{
  "schema_version": 1,
  "base_url": "http://127.0.0.1:NNNNN",
  "capability_token": "..."
}
```

Unix endpoint file 强制 0600；Windows 写入时移除继承 ACL 并只授予当前 Windows principal；token 不打印到 stdout。`/v1/models`、`/v1/responses` 和 `/v1/chat/completions` 都要求 `x-codex-omnibridge-token: <raw token>`；官方 route 另外要求有效的 `Authorization: Bearer ...`，该 header 只允许转发到官方 upstream，Custom route 不携带它。POST body 必须是 `application/json`。官方 model ID 被 Router 明确拒绝为 `501 Not Implemented`，避免把官方请求误发给第三方适配层。

当前仍是 loopback TCP，不是 Unix socket / Windows named pipe。Router 已对 Host 和
Origin 做 loopback 校验，并保留 capability token、JSON body 上限和 endpoint 文件权限
校验；Windows IPC、跨进程生命周期恢复和真实 Desktop caller 仍需要目标机验收，随机
token 不能替代这些边界。

## 协议与 Context Boundary

当前实现以 Codex Responses 请求为主路径：Responses provider 的请求和 SSE 响应可直接
转发；Chat Completions provider 的非流式请求会做显式的消息转换。跨协议流式转换会
返回 `400`，因为无法在没有完整事件语义映射的情况下保证 tool、reasoning、cancel 和
错误事件的含义。

输入中的图片、文件、hosted tool、函数调用历史和缺失 reasoning summary 会依据目标
provider 能力进行校验；不支持的内容返回带有 `context boundary` 的错误，不会静默删
除、替换成占位文本或把官方专属字段发送给第三方。这个边界目前有 Router 单元测试，
仍需要真实第三方 Responses/Chat Provider 分别验收。

## 配置修改和 fail-safe manifest

集成目录默认使用 `default_registry_path()` 的父目录：

```text
providers.json
models.json
integration.json
router-endpoint.json
```

`integration.json` 记录实际 Codex config 路径、被管理字段、应用值、原值、catalog 路径以及 Codex binary/version。配置编辑只定位 root table 的 `model_catalog_json`、`model_provider` 和 `model_providers.omnibridge`，不修改 `[profiles.*]` 等后续 TOML table，并保留 LF/CRLF。

可用 `scripts/stock-omnibridge-e2e.py` 的 `--cli-bin` 选项同时验证 stock
`codex exec`；脚本会在 app-server 验证结束后关闭它，再用相同的隔离
`CODEX_HOME` 验证终端 CLI，避免把 SQLite 锁或全局用户数据混入结果。

restore 首先验证当前值仍等于 manifest 的 `applied_value`：

- 相等：恢复原字段或移除原本不存在的字段，并删除生成 catalog；
- 不相等：返回 `UserChangedManagedField`，不覆盖用户修改；
- manifest schema、字段名或原值不一致：拒绝执行。

## 证据分类与当前边界

| 项目 | 当前状态 | 证据边界 |
|---|---|---|
| workspace route 判定 | `PASS` | core/router 单元测试 |
| Router token/endpoint 安全 | `PASS` | Router tests + 0600 endpoint implementation |
| Router Host/Origin loopback gate | `PASS` | external Host/Origin rejection tests |
| release CLI Router process HTTP smoke | `PASS` | release `codex-mp` + loopback mock provider；healthz、model list、model rewrite、provider path/auth、mock response |
| Catalog future-field allowlist | `PASS` | custom entry test rejects hosted/unknown capability fields |
| Context Boundary explicit rejection | `PASS` | image/file/hosted-tool/streaming bridge tests |
| Provider/model 面板命令与托盘生命周期骨架 | `PASS` | Tauri source + JS/config checks；native build 受宿主依赖阻塞 |
| Linux CLI 安装/恢复/卸载源码路径 | `PASS` | shell syntax + CLI temp-dir smoke；未执行真实 Linux 目标机安装 |
| patched `codex-core` 编译 | `PASS` | pinned upstream `cargo check` / logical-routing test |
| patched Core logical-routing test | `PASS` | 上游 `cargo test -p codex-core logical_routing` |
| stock `codex exec` custom turn | `PASS` | stock CLI 在隔离 `CODEX_HOME` 中完成 `newapi/qwen3.8`；Provider 收到 `qwen3.8`、第三方 Bearer 和流式请求，未收到 Router capability，CLI 退出码为 0 |
| 真实 CLI/app-server custom turn | `PASS` | pinned patched `codex-app-server` 与 `codex exec` 在隔离 `CODEX_HOME` 中完成 custom turn；Router capability 200/401、Provider model rewrite、无官方 Authorization 均有请求日志证据 |
| 同 Thread Official → Custom → Official | `PASS`（受控 fixture） | 同一 app-server thread 三轮均 `completed`；官方 mock 只收到测试 bearer，Custom mock 只收到 `fixture-model` 且没有官方 Authorization |
| Desktop runtime adapter source | `PASS` | 三平台发现、manifest/backup/launcher/restore、Windows/macOS 目标编译；Windows 最新脚本 parser 已在 VM 执行 |
| macOS native Desktop adapter install/status/restore | `PASS` | macOS 26.6.1 VM 原生编译；fake app-server install/status/restore、launcher exit、installer roundtrip、官方 app codesign 通过 |
| Windows AppX/runtime/installer smoke | `PASS` | Windows 11 VM baseline 的真实 AppX/runtime 发现、PowerShell parser、安装/卸载脚本 roundtrip、交互式 ChatGPT.exe 启动通过；最新 build/installer/uninstaller 三份 PowerShell parser、交叉构建 PE CLI 和 external-artifact fixture roundtrip 均通过，真实 patched Codex artifact 仍未运行 |
| Windows native `codex-mp.exe --help` | `PASS` | `x86_64-pc-windows-gnu` 交叉构建的 PE 在 Windows 11 VM 执行通过 |
| Windows native `codex-mp` adapter install/launcher turn | `NOT PROVEN` | 本项目 Windows CLI PE 已在 VM 执行 `--help`，但尚无真实 patched Codex CLI/app-server artifact，因此 launcher turn 仍未证明 |
| Desktop/Bridge F1 | `NOT PROVEN` | 需目标机真实 Desktop caller、picker 和 app-server turn |
| F2/F3/F4/F5 | `NOT PROVEN` | 需真实 UI、Thread、Context、tool E2E |
| Remote app-server | `NOT RUN` | 未建立远程 Host 测试环境 |
| 真实 NewAPI/OpenRouter | `NOT RUN` | 未使用真实第三方账号/API |
| Upgrade/Repair/Uninstall 目标机生命周期 | `NOT RUN` | 源码与临时目录冒烟已通过，未在真实安装环境执行 |
| patched `codex-cli`/`codex-app-server` release artifact | `PASS`（Linux） | pinned commit 低并发 release build 完成；`codex --help`/`codex-app-server --help` 通过，SHA256 分别为 `16b5c00f60c874043ee1cd38bff5a52d812620ce13c3b1cff94cdac55354b1f9`、`db454162f97399c14be383a20ccc099f0cf4cf34554ebe8ca7afae24dc471111`；Windows/macOS 原生 patched artifact 仍 `NOT RUN` |

因此，以上受控 fixture 证据足以证明 Linux patched caller 的真实数据面，但不能替代真实第三方账号、真实 Desktop caller、Remote Host 或 Windows/macOS 原生 patched artifact 验收。
