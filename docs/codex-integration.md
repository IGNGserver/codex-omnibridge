# Codex 集成说明

## 结论先行

本项目的集成分成两个层次：

1. **Catalog 层**：把 namespaced custom model 加入当前安装版 Codex 的 `model_catalog_json`，让原生 Model Picker / app-server `model/list` 能看到它。
2. **Core 数据面层**：对固定上游 Codex commit 应用 `patches/codex/73a1148c9c775c2a4616ce5096291740a00ed68a/0001-per-turn-local-router.patch`，让真实 `ModelClientSession::stream()` 按每轮 logical model 选择官方 backend 或本地 Router。

第二层已经以可复现的 patched Core 代码落地并通过上游 `codex-core` 编译和 logical-routing 单元测试；本仓库也已补上 Linux CLI 安装器、可回滚卸载、Router supervisor 和 Tauri 面板/托盘骨架。但是本仓库没有把上游 Codex 二进制重新打包，也没有真实 ChatGPT Account、第三方 Provider、Desktop 或 Remote app-server 的运行验收。因此总体产品结论仍是 `FAIL` / `NOT PROVEN`，而不是把源码和 mock 证据写成产品完成。

## 不变量

实现刻意不修改：

- `model_provider`，仍为 `openai`；
- `chatgpt_base_url`、`openai_base_url`；
- `auth.json`、OAuth token 或 ChatGPT Account 状态；
- Thread provider、Thread ID 或 Thread 存储；
- 官方模型的原生 backend 路径。

API key 只通过 `codex-mp-credentials` 的 keyring 实现读取。普通 registry、catalog、manifest、日志和错误消息不保存 API key。Custom Core 请求使用 Router capability token；ChatGPT OAuth 不作为第三方 Provider 的 Authorization 转发。

## Catalog 来源与合并

`codex-mp sync` 和 `codex-mp repair` 执行当前传入 binary 的：

```text
codex debug models --bundled
```

默认 binary 是 `codex`，可通过 `--codex-bin /absolute/path/to/codex` 覆盖。输出必须是根对象并包含 `models` 数组。合并器复制官方 JSON 对象再覆盖必要字段，保留官方未来可能增加的未知字段。

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
            → Authorization: Bearer <Router capability token>
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

`apply.sh` 只接受完全匹配的上游 HEAD，并使用 `git apply --check`；不会 reset、clean、覆盖用户修改或自动提交。补丁目录的 README 记录文件列表、SHA-256、构建约束和已知限制。

### 启动顺序

先启动 Router：

```bash
ENDPOINT_FILE="$HOME/.config/codexmultiprovider/router-endpoint.json"
codex-mp router --port 0 --endpoint-file "$ENDPOINT_FILE"
```

再让 patched Codex CLI/app-server 继承同一 endpoint file：

```bash
export CODEX_MP_ROUTER_ENDPOINT_FILE="$ENDPOINT_FILE"
/path/to/patched/codex app-server
```

如果 Desktop/Bridge 不是从该 shell 继承环境，必须在其进程启动配置中设置同一变量。Core 会在 `ModelClient` 初始化时读取 endpoint；文件缺失或权限/URL/token 不合法时，Custom turn 明确失败，Official turn 不被改道。

## Router 安全边界

Router 默认只绑定 `127.0.0.1`，端口可以为 `0`。启动后写入 JSON endpoint file：

```json
{
  "schema_version": 1,
  "base_url": "http://127.0.0.1:NNNNN",
  "capability_token": "..."
}
```

Unix endpoint file 强制 0600；token 不打印到 stdout。`/v1/models`、`/v1/responses` 和 `/v1/chat/completions` 都要求 `Authorization: Bearer ...`；POST body 必须是 `application/json`。官方 model ID 被 Router 明确拒绝为 `501 Not Implemented`，避免把官方请求误发给第三方适配层。

当前仍是 loopback TCP，不是 Unix socket / Windows named pipe；Host/Origin 强化、Windows IPC 和完整生命周期属于后续工作，不能把随机 token 描述成所有本地攻击面的解决方案。

## 配置修改和 fail-safe manifest

集成目录默认使用 `default_registry_path()` 的父目录：

```text
providers.json
models.json
integration.json
router-endpoint.json
```

`integration.json` 记录实际 Codex config 路径、被管理字段、应用值、原值、catalog 路径以及 Codex binary/version。配置编辑只定位 root table 的 `model_catalog_json`，不修改 `[profiles.*]` 等后续 TOML table，并保留 LF/CRLF。

restore 首先验证当前值仍等于 manifest 的 `applied_value`：

- 相等：恢复原字段或移除原本不存在的字段，并删除生成 catalog；
- 不相等：返回 `UserChangedManagedField`，不覆盖用户修改；
- manifest schema、字段名或原值不一致：拒绝执行。

## 证据分类与当前边界

| 项目 | 当前状态 | 证据边界 |
|---|---|---|
| workspace route 判定 | `PASS` | core/router 单元测试 |
| Router token/endpoint 安全 | `PASS` | Router tests + 0600 endpoint implementation |
| Provider/model 面板命令与托盘生命周期骨架 | `PASS` | Tauri source + JS/config checks；native build 受宿主依赖阻塞 |
| Linux CLI 安装/恢复/卸载源码路径 | `PASS` | shell syntax + CLI temp-dir smoke；未执行真实安装 |
| patched `codex-core` 编译 | `PASS` | 上游 `cargo check -p codex-core` |
| patched Core logical-routing test | `PASS` | 上游 `cargo test -p codex-core logical_routing` |
| 真实 CLI/app-server custom turn | `NOT PROVEN` | 尚无带第三方 mock 的真实 turn |
| 同 Thread Official → Custom → Official | `NOT PROVEN` | 尚无真实 Thread caller 证据 |
| Desktop/Bridge F1 | `NOT PROVEN` | 尚无 Desktop caller/安装产物 |
| F2/F3/F4/F5 | `NOT PROVEN` | 需真实 UI、Thread、Context、tool E2E |
| Remote app-server | `NOT RUN` | 未建立远程 Host 测试环境 |
| 真实 NewAPI/OpenRouter | `NOT RUN` | 未使用真实第三方账号/API |
| Upgrade/Repair/Uninstall 目标机生命周期 | `NOT RUN` | 源码与临时目录冒烟已通过，未在真实安装环境执行 |

因此，Catalog 可见、Router standalone 请求、Core 源码存在、Core 单元测试通过，都不能单独改写以上结论。
