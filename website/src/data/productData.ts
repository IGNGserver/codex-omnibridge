export type RoutePathKind = 'official' | 'bridge' | 'local';

export interface ModelRoutePreset {
  id: string;
  shortLabel: string;
  vendorBadge: string;
  logicalModelId: string;
  upstreamModelId: string;
  routeKind: RoutePathKind;
  routeBadge: string;
  accentHex: string;
  accentRgb: [number, number, number];
  secondaryHex: string;
  protocolLabel: string;
  authAction: string;
  authDetail: string;
  inboundEndpoint: string;
  egressEndpoint: string;
  bridgePipeline: string;
  contextWindow: string;
  capabilities: string[];
  gimbalYawRad: number;
  gimbalPitchRad: number;
  vaultActive: boolean;
  summary: string;
}

export const MODEL_ROUTE_PRESETS: ModelRoutePreset[] = [
  {
    id: 'gpt-official',
    shortLabel: 'GPT-5.4 Official',
    vendorBadge: 'OFFICIAL · OAUTH',
    logicalModelId: 'gpt-5.4',
    upstreamModelId: 'gpt-5.4',
    routeKind: 'official',
    routeBadge: 'PATH α · 官方零转换直通',
    accentHex: '#00daf2',
    accentRgb: [0.0, 0.855, 0.949],
    secondaryHex: '#96f0ff',
    protocolLabel: 'Native Responses SSE (Zero Translation)',
    authAction: 'PASS ChatGPT OAuth · 剥离本地 Capability',
    authDetail: '保留 Authorization / ChatGPT-Account-Id / session-id / x-codex-* 白名单头；绝不改写 auth.json',
    inboundEndpoint: 'POST http://127.0.0.1:8787/v1/responses',
    egressEndpoint: 'https://chatgpt.com/backend-api/codex/responses',
    bridgePipeline: 'Bypass ProtocolBridge → 原生保留 Hosted Search / 生图 / V8 Code Mode / Compact',
    contextWindow: '400K',
    capabilities: ['原生订阅额度', 'Hosted Tools', '图像生成', 'Reasoning xhigh', 'SSE 原样透传'],
    gimbalYawRad: -0.44,
    gimbalPitchRad: 0.08,
    vaultActive: false,
    summary:
      '命中ProviderRegistry官方模型表，光束偏转入蓝宝石真空直通轨（Channel α），不经任何第三方协议转换层，完整保留官方登录态与专属特权。',
  },
  {
    id: 'claude-openrouter',
    shortLabel: 'Claude Opus 4.6',
    vendorBadge: 'OPENROUTER · BRIDGE',
    logicalModelId: 'openrouter/claude-opus-4.6',
    upstreamModelId: 'anthropic/claude-opus-4.6',
    routeKind: 'bridge',
    routeBadge: 'PATH β · 协议棱镜桥接',
    accentHex: '#d97bff',
    accentRgb: [0.851, 0.482, 1.0],
    secondaryHex: '#fdd6ff',
    protocolLabel: 'Responses API ⇄ Chat Completions SSE',
    authAction: 'STRIP 官方 OAuth · 注入 Keyring Bearer',
    authDetail: '彻底剥离 ChatGPT Authorization 与 Account-Id；从系统钥匙串读取 dev.codex-multiprovider 凭据',
    inboundEndpoint: 'POST http://127.0.0.1:8787/v1/responses',
    egressEndpoint: 'https://openrouter.ai/api/v1/chat/completions',
    bridgePipeline: 'portable_request 净化 → CodexToolContext 可逆工具编码 → reasoning.effort 映射 → SSE 重组',
    contextWindow: '200K',
    capabilities: ['可逆 apply_patch', 'MCP 命名空间工具', '多模态截图提取', 'OpenRouter 思考映射'],
    gimbalYawRad: 0.0,
    gimbalPitchRad: 0.0,
    vaultActive: true,
    summary:
      '光束直射三联布儒斯特棱镜组（Protocol Bridge），实时将 Codex 专属 Responses 请求与自定义语法工具编码为标准 Chat Completions 流，并由钥匙箱注入独立密钥。',
  },
  {
    id: 'qwen-newapi',
    shortLabel: 'Qwen4 Max',
    vendorBadge: 'NEWAPI · BRIDGE',
    logicalModelId: 'newapi/qwen4-max',
    upstreamModelId: 'qwen4-max',
    routeKind: 'bridge',
    routeBadge: 'PATH β · 协议棱镜桥接',
    accentHex: '#feb958',
    accentRgb: [0.996, 0.725, 0.345],
    secondaryHex: '#ffddb6',
    protocolLabel: 'Responses API ⇄ Chat Completions SSE',
    authAction: 'STRIP 官方 OAuth · 注入 Keyring Bearer',
    authDetail: '跨进程代际缓存（registry.generation）零延迟取钥；禁止 302/307/308 重定向防凭据外泄',
    inboundEndpoint: 'POST http://127.0.0.1:8787/v1/responses',
    egressEndpoint: 'https://api.newapi.internal/v1/chat/completions',
    bridgePipeline: 'UTF-8 跨包边界安全流解析 → thinking/reasoning_effort 双栈适配 → 逻辑模型名还原',
    contextWindow: '128K',
    capabilities: ['中文跨帧防截断', 'Flat Tools 展平', '思考摘要流', '流式/非流式自适应'],
    gimbalYawRad: 0.04,
    gimbalPitchRad: -0.05,
    vaultActive: true,
    summary:
      '针对 NewAPI / OneAPI 聚合网关深度硬化：即使上游忽略 stream:true 返回整包 JSON，或将中文多字节字符切分在 TCP 帧边界，桥接器亦能无损还原为 Codex 标准事件流。',
  },
  {
    id: 'deepseek-oneapi',
    shortLabel: 'DeepSeek V4.1',
    vendorBadge: 'ONEAPI · REASONER',
    logicalModelId: 'oneapi/deepseek-v4.1',
    upstreamModelId: 'deepseek-v4.1',
    routeKind: 'bridge',
    routeBadge: 'PATH β · 深度思考桥接',
    accentHex: '#5cb3ff',
    accentRgb: [0.361, 0.702, 1.0],
    secondaryHex: '#d6e3ff',
    protocolLabel: 'Responses API ⇄ DeepSeek Reasoner SSE',
    authAction: 'STRIP 官方 OAuth · 注入 Keyring X-API-Key / Bearer',
    authDetail: '清除 encrypted_content 与 prompt_cache_key，将可读思考摘要转化为可移植上下文',
    inboundEndpoint: 'POST http://127.0.0.1:8787/v1/responses',
    egressEndpoint: 'https://oneapi.team.example/v1/chat/completions',
    bridgePipeline: 'build_reasoning_config(deepseek) → reasoning_content 增量转 reasoning_summary_text.delta',
    contextWindow: '128K',
    capabilities: ['DeepSeek 思考链提取', '600s 逐块读超时', '工具参数流式拼接', '跨模型历史注水'],
    gimbalYawRad: -0.04,
    gimbalPitchRad: 0.05,
    vaultActive: true,
    summary:
      '自动识别 DeepSeek 推理特征，把上游 reasoning_content 增量流实时映射为 Codex 原生思考摘要事件，且采用 600 秒逐块读超时机制，长时深度推理绝不中途断流。',
  },
  {
    id: 'gemini-newapi',
    shortLabel: 'Gemini 3.8 Flash',
    vendorBadge: 'NEWAPI · MULTIMODAL',
    logicalModelId: 'newapi/gemini-3.8-flash',
    upstreamModelId: 'gemini-3.8-flash',
    routeKind: 'bridge',
    routeBadge: 'PATH β · 多模态工具桥接',
    accentHex: '#2ee8bb',
    accentRgb: [0.18, 0.91, 0.733],
    secondaryHex: '#b8fff0',
    protocolLabel: 'Responses API ⇄ Multimodal Chat SSE',
    authAction: 'STRIP 官方 OAuth · 注入 Keyring Bearer',
    authDetail: 'tool_media 模块自动剥离工具输出中的图片/PDF，合成紧随其后的多模态 user 消息',
    inboundEndpoint: 'POST http://127.0.0.1:8787/v1/responses',
    egressEndpoint: 'https://api.newapi.internal/v1/chat/completions',
    bridgePipeline: 'extract_tool_media 多模态提升 → 1M Context 目录声明 → Responses 生命周期闭合',
    contextWindow: '1,048K',
    capabilities: ['1M 超长上下文', '工具返回图片可见', 'PDF/音频附件桥接', '极速首字响应'],
    gimbalYawRad: 0.02,
    gimbalPitchRad: 0.03,
    vaultActive: true,
    summary:
      '解决标准 Chat Completions 中 tool 角色无法携带图像的协议死穴：tool_media 引擎自动将工具截图与文档提取为紧邻的多模态消息，让百万上下文视觉模型真正看懂终端截图。',
  },
  {
    id: 'ollama-local',
    shortLabel: 'Ollama · Qwen3 Coder',
    vendorBadge: '127.0.0.1 · LOCAL',
    logicalModelId: 'ollama/qwen3-coder',
    upstreamModelId: 'qwen3-coder:30b',
    routeKind: 'local',
    routeBadge: 'PATH γ · 本机回环零外发',
    accentHex: '#4ef09c',
    accentRgb: [0.306, 0.941, 0.612],
    secondaryHex: '#c2efb1',
    protocolLabel: 'Loopback HTTP Responses ⇄ Local Chat SSE',
    authAction: 'STRIP 官方 OAuth · AuthStrategy::None',
    authDetail: 'normalize_base_url 仅对 loopback 豁免 HTTPS 强制要求，零外部 DNS 解析、零云端流量',
    inboundEndpoint: 'POST http://127.0.0.1:8787/v1/responses',
    egressEndpoint: 'http://127.0.0.1:11434/v1/chat/completions',
    bridgePipeline: 'Loopback 闭环谐振腔 → 禁用 V8 code_mode_only 改发标准 Flat Tools → 本地推理流回传',
    contextWindow: '128K',
    capabilities: ['100% 离线不出网', 'AuthStrategy::None', 'Ollama / vLLM / LM Studio', '零订阅消耗'],
    gimbalYawRad: 0.46,
    gimbalPitchRad: -0.06,
    vaultActive: false,
    summary:
      '光束偏转入铜绕环形本机谐振腔（Channel γ），请求全程停留在本机 127.0.0.1 回环网络内直连 Ollama / vLLM / LM Studio，兼顾敏感代码离线审计与官方会话无缝衔接。',
  },
];

export interface InternalModuleStage {
  id: string;
  indexLabel: string;
  crateName: string;
  nodeName: string;
  title: string;
  subtitle: string;
  accentHex: string;
  focusWorld: [number, number, number];
  cameraPos: [number, number, number];
  cameraLookAt: [number, number, number];
  canopyOpenFactor: number;
  telemetryCode: string;
  specs: Array<{ label: string; value: string }>;
  bullets: string[];
}

export const INTERNAL_MODULE_STAGES: InternalModuleStage[] = [
  {
    id: 'stage-catalog',
    indexLabel: 'MODULE 01 // CATALOG SYNTHESIS',
    crateName: 'crates/catalog · crates/integration',
    nodeName: 'Module_Registry',
    title: '模型注册与无损目录合成',
    subtitle: '从本机 Codex 二进制提取权威 Schema，将自定义模型注入原生 /model 选择器',
    accentHex: '#00daf2',
    focusWorld: [-1.52, 0.16, 0.0],
    cameraPos: [-2.35, 1.42, 2.55],
    cameraLookAt: [-1.35, 0.05, 0.0],
    canopyOpenFactor: 0.85,
    telemetryCode: 'codex debug models --bundled → SHA-256 schema_fingerprint',
    specs: [
      { label: '配置编辑引擎', value: 'toml_edit 保注释两阶段提交' },
      { label: '工具模式净化', value: '关闭 V8 code_mode_only → 标准 Flat Tools' },
      { label: '身份提示词脱敏', value: '中性化 OpenAI 专属自称，保留工具规范' },
    ],
    bullets: [
      '自动执行 `codex debug models --bundled` 读取当前安装版本的真实内建目录，未知上游字段原样保留，绝不因 Codex 升级丢字段。',
      '通过字段相似度打分挑选通用官方模板克隆自定义模型，剥离第三方无法执行的 `web_search` 与 V8 沙箱模式，让 `exec_command` 与 `apply_patch` 稳定工作。',
      '`integration.json` 记录安装前 SHA-256 指纹；若检测到用户手工改动受管配置，`restore` 拒绝盲目覆盖（Fail-Closed）。',
    ],
  },
  {
    id: 'stage-router',
    indexLabel: 'MODULE 02 // PER-TURN OPTICAL CROSSBAR',
    crateName: 'crates/router · crates/manager',
    nodeName: 'Module_Crossbar',
    title: '逐轮精确光路分流器',
    subtitle: '突破 Stock Codex 会话级 Provider 锁死，同一 Thread 内每轮动态路由',
    accentHex: '#96f0ff',
    focusWorld: [-0.36, 0.22, 0.0],
    cameraPos: [-0.45, 1.65, 2.35],
    cameraLookAt: [-0.25, 0.06, 0.0],
    canopyOpenFactor: 1.0,
    telemetryCode: '127.0.0.1:8787 · x-codex-omnibridge-token · Constant-Time Verify',
    specs: [
      { label: '监听边界', value: '严格 Loopback (拒绝 0.0.0.0)' },
      { label: '路由判定依据', value: 'ProviderRegistry 精确表查（非斜杠猜测）' },
      { label: '并发流控护栏', value: 'zstd 解压 16MiB 上限 + Semaphore 背压' },
    ],
    bullets: [
      'Stock Codex 仅需绑定单一 `omnibridge` Responses 入口；Router 在每一轮请求体中精确匹配 `official_models` 与 `<provider>/<slug>`。',
      '官方模型直通 `chatgpt.com/backend-api/codex`，精确放行 `originator`、`user-agent`、`x-openai-internal-codex-responses-lite` 等实测头。',
      '内置 2 秒周期 `RouterSupervisor` 健康巡检：发现 `registry_generation` 落后时优先热重载 `/admin/reload`，异常崩溃时指数退避自愈。',
    ],
  },
  {
    id: 'stage-bridge',
    indexLabel: 'MODULE 03 // BREWSTER PROTOCOL PRISM',
    crateName: 'crates/protocol-bridge',
    nodeName: 'Module_ProtocolBridge',
    title: '双向协议棱镜与跨模型记忆水合',
    subtitle: '实时互译 Responses API 与 Chat Completions，无损衔接多轮工具与思考流',
    accentHex: '#d97bff',
    focusWorld: [1.08, 0.20, 0.0],
    cameraPos: [1.18, 1.48, 2.32],
    cameraLookAt: [0.95, 0.06, 0.0],
    canopyOpenFactor: 1.0,
    telemetryCode: 'CodexToolContext ⇄ chat_sse_to_responses_stream ⇄ CodexChatHistoryStore',
    specs: [
      { label: '工具形态可逆编码', value: 'function / custom(语法) / namespace / search' },
      { label: '跨边界状态净化', value: 'portable_request 剥离 encrypted_content' },
      { label: '多模态工具输出', value: 'tool_media 自动提升图片/PDF/音频消息' },
    ],
    bullets: [
      '`CodexToolContext` 将 `apply_patch` 等自由语法工具与 MCP 命名空间工具无损降维为标准 function，并在模型响应时精确还原原始调用类型与 `call_id`。',
      '`CodexChatHistoryStore` 在内存中维护有界会话账本：当同一对话从官方模型切向第三方模型时自动水合上下文，消除无状态接口断片。',
      '`append_utf8_safe_bounded` 逐字节守护 TCP 分块边界，彻底根除高并发 SSE 流下中文与 Emoji 被切碎导致的解码错误。',
    ],
  },
  {
    id: 'stage-vault',
    indexLabel: 'MODULE 04 // FARADAY KEYCHAIN VAULT',
    crateName: 'crates/credentials · crates/web',
    nodeName: 'Module_KeychainVault',
    title: '系统钥匙箱与物理级凭据隔离',
    subtitle: 'API Key 永不落明文配置，官方 OAuth 永不越界，307/308 重定向硬阻断',
    accentHex: '#feb958',
    focusWorld: [-0.10, 0.24, 1.25],
    cameraPos: [0.65, 1.56, 2.85],
    cameraLookAt: [0.05, 0.08, 0.72],
    canopyOpenFactor: 0.92,
    telemetryCode: 'OS Keyring (dev.codex-multiprovider) · redirect::Policy::none()',
    specs: [
      { label: '原生凭据后端', value: 'macOS Keychain / Win CredMan / Secret Service' },
      { label: '防泄漏 HTTP 策略', value: '全局禁用重定向 + 阻断 169.254.0.0/16' },
      { label: '面板远程安全', value: 'Argon2id 哈希 + 登录限流 + 严格 CSP' },
    ],
    bullets: [
      '`providers.json` 与 `accounts.json` 仅保存不透明引用指针；Windows 超长 OAuth 令牌（>=2560 UTF-16）自动回退至当前用户 SID DACL 保护的私有存储。',
      '上游 HTTP 客户端强制关闭重定向（`Policy::none()`）：杜绝恶意或被劫持中转站通过 `307/308` 诱导客户端把 Bearer 密钥与对话正文重发至第三方主机。',
      '可选「官方账号保险箱」在每次切换 ChatGPT Plus/Pro/Team 账号前自动生成 `0600` 权限 `auth.bak-switch-*` 快照，随时一键无损回退。',
    ],
  },
];

export interface ThreadTurnStep {
  turnIndex: number;
  commandOrPrompt: string;
  slashSwitch?: string;
  modelId: string;
  presetId: string;
  badge: string;
  routeSummary: string;
  headersSent: string[];
  headersStripped: string[];
  assistantPreview: string;
  latencyTag: string;
}

export const THREAD_SIMULATION_STEPS: ThreadTurnStep[] = [
  {
    turnIndex: 1,
    commandOrPrompt: '请结合最新仓库结构，定位 crates/router 的鉴权入口并搜索相关 RFC 规范。',
    modelId: 'gpt-5.4',
    presetId: 'gpt-official',
    badge: 'TURN 01 · OFFICIAL OAUTH',
    routeSummary: '命中 official_models → 直通 chatgpt.com/backend-api/codex（保留 Hosted Web Search 与原生订阅态）',
    headersSent: [
      'Authorization: Bearer <ChatGPT_Plus_OAuth>',
      'ChatGPT-Account-Id: acc_8f92...d1',
      'x-openai-internal-codex-responses-lite: true',
      'thread-id: thr_019a8c...4e',
    ],
    headersStripped: ['x-codex-omnibridge-token (本地环回鉴权头已剥离)'],
    assistantPreview:
      '已通过官方 Hosted Search 核对规范，并定位 `crates/router/src/lib.rs`：所有请求均经 `CAPABILITY_HEADER` 常量时间校验后按注册表精确分流……',
    latencyTag: 'TTFT 310ms · Native SSE',
  },
  {
    turnIndex: 2,
    slashSwitch: '/model openrouter/claude-opus-4.6',
    commandOrPrompt: '很好，直接在当前会话为该模块补齐边界单元测试并用 apply_patch 写入。',
    modelId: 'openrouter/claude-opus-4.6',
    presetId: 'claude-openrouter',
    badge: 'TURN 02 · SAME-THREAD SWITCH → CLAUDE',
    routeSummary: '同 Thread 切换！CodexChatHistoryStore 自动水合第 1 轮上下文 → 剥离 OAuth → 协议棱镜转译 apply_patch',
    headersSent: [
      'Authorization: Bearer <Keyring:dev.codex-multiprovider/openrouter>',
      'Content-Type: application/json',
    ],
    headersStripped: [
      'Authorization: Bearer <ChatGPT_Plus_OAuth> (物理阻断)',
      'ChatGPT-Account-Id (物理阻断)',
      'previous_response_id / encrypted_content (跨源净化)',
    ],
    assistantPreview:
      '⚡ [CodexToolContext 已将 custom 语法工具 `apply_patch` 编码并还原] 已在 `crates/router/src/lib.rs` 新增 3 组常量时间鉴权与非环回拒绝断言。',
    latencyTag: 'TTFT 480ms · Bridged SSE',
  },
  {
    turnIndex: 3,
    slashSwitch: '/model ollama/qwen3-coder',
    commandOrPrompt: '在不联网的前提下审查刚才这段测试是否包含任何真实密钥或内网地址。',
    modelId: 'ollama/qwen3-coder',
    presetId: 'ollama-local',
    badge: 'TURN 03 · LOCAL LOOPBACK AUDIT',
    routeSummary: '切换至本机 127.0.0.1:11434 Ollama 闭环谐振腔，零外部网络请求、零认证头发送',
    headersSent: ['Content-Type: application/json (AuthStrategy::None)'],
    headersStripped: [
      'Authorization (全部剥离)',
      'ChatGPT-Account-Id (全部剥离)',
      'x-codex-* (全部剥离)',
    ],
    assistantPreview:
      '🔒 [本机离线完成] 静态扫描完毕：测试夹具仅使用 `MemoryCredentialStore` 与 `127.0.0.1` 环回地址，无任何凭据字面量泄漏。',
    latencyTag: 'Local 127.0.0.1 · 0 Bytes Egress',
  },
  {
    turnIndex: 4,
    slashSwitch: '/model gpt-5.4',
    commandOrPrompt: '切回官方模型，总结本轮变更并生成一张架构示意草图。',
    modelId: 'gpt-5.4',
    presetId: 'gpt-official',
    badge: 'TURN 04 · SEAMLESS RETURN TO OFFICIAL',
    routeSummary: '零重启切回官方通道！ChatGPT 登录态完好无损，原生 `/v1/images/generations` 与订阅额度继续生效',
    headersSent: [
      'Authorization: Bearer <ChatGPT_Plus_OAuth>',
      'ChatGPT-Account-Id: acc_8f92...d1',
      'thread-id: thr_019a8c...4e',
    ],
    headersStripped: ['第三方 Provider API Key 绝不混入官方请求'],
    assistantPreview:
      '✨ 已无缝回到同一官方 Thread：全部 4 轮上下文连贯保留，官方生图与代码解释器特权随时可用，无需重登或重开窗口。',
    latencyTag: 'TTFT 295ms · Official Restored',
  },
];

export interface CrateSpecItem {
  crate: string;
  layer: string;
  responsibility: string;
  highlight: string;
}

export const WORKSPACE_CRATES: CrateSpecItem[] = [
  {
    crate: 'codex-mp-core',
    layer: 'Domain & Security',
    responsibility: '注册表模型、代际计数、HTTPS/Loopback URL 校验、跨进程文件锁与 0600/SID 原子写',
    highlight: '阻断 169.254.0.0/16 SSRF 与明文非环回 HTTP',
  },
  {
    crate: 'codex-mp-catalog',
    layer: 'Schema Merger',
    responsibility: '调用 codex debug models --bundled 提取真实目录，模板克隆并中性化 V8 专属工具标志',
    highlight: 'SHA-256 Schema 指纹守护，未知字段零丢失',
  },
  {
    crate: 'codex-mp-credentials',
    layer: 'Secret Vault',
    responsibility: '统一封装 macOS Keychain、Windows Credential Manager 与 Linux Secret Service',
    highlight: '8 槽异步隔离池 + Windows 超长凭据自动 0600 溢出回退',
  },
  {
    crate: 'codex-mp-protocol-bridge',
    layer: 'Wire Translator',
    responsibility: 'Responses ⇄ Chat Completions 请求/SSE 双向转换、可逆工具编码、多模态提取与历史账本',
    highlight: '移植自 CC Switch v3.20.3 并深度硬化 UTF-8 与跨源净化',
  },
  {
    crate: 'codex-mp-router',
    layer: 'Loopback Data Plane',
    responsibility: '127.0.0.1:8787 高安全反向代理，逐轮表驱动分流、zstd 解压、零重定向与代际密钥缓存',
    highlight: '常量时间 Capability 校验 + 600s 逐块流式读超时',
  },
  {
    crate: 'codex-mp-integration',
    layer: 'Config Transaction',
    responsibility: '基于 toml_edit 无损更新 ~/.codex/config.toml，维护 integration.json 两阶段事务清单',
    highlight: '检测到人工修改受管字段时 Fail-Closed 拒绝覆盖',
  },
  {
    crate: 'codex-mp-manager',
    layer: 'Supervisor & Accounts',
    responsibility: 'Provider 自动发现、Router 进程守护自愈、官方账号快照切换与 5h/7d/Reserve 额度查询',
    highlight: '每次切号自动留存 auth.bak-switch-* 可恢复备份',
  },
  {
    crate: 'codex-mp-web',
    layer: 'Control Plane SPA',
    responsibility: 'RustEmbed 内嵌 M3 Expressive 面板（默认 127.0.0.1:31828），Argon2id 鉴权与登录限流',
    highlight: '未设密码硬性拒绝 0.0.0.0 局域网绑定 + 严格双端一致 CSP',
  },
  {
    crate: 'codex-mp-desktop',
    layer: 'Experimental Adapter',
    responsibility: '可选独立 Desktop 运行时启动器（CODEX_CLI_PATH / POSIX 包装器）与哈希漂移检测',
    highlight: '一键 codex-mp desktop restore 恢复官方原厂入口',
  },
  {
    crate: 'codex-mp-cli',
    layer: 'Unified Binary',
    responsibility: '单一原生可执行文件 codex-mp，涵盖 setup / sync / launch / resume / doctor / web / tray',
    highlight: '零 GTK/WebKit 系统依赖，管道传入密钥历史零留痕',
  },
];
