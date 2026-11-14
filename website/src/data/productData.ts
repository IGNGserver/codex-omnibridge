export type RoutePathKind = 'official' | 'bridge' | 'local';

export interface ModelRoutePreset {
  id: string;
  shortLabel: string;
  vendorBadge: string;
  logicalModelId: string;
  upstreamModelId: string;
  routeKind: RoutePathKind;
  routeBadge: string;
  /** Accessible foreground accent in Light site theme */
  accentLight: string;
  /** Accessible foreground accent in Dark site theme */
  accentDark: string;
  /** Distinct vibrant color for 3D waveguides & indicator dots */
  waveHex: string;
  protocolLabel: string;
  authAction: string;
  authDetail: string;
  egressEndpoint: string;
  capabilities: string[];
  gimbalYawRad: number;
  gimbalPitchRad: number;
  vaultActive: boolean;
  summary: string;
}

export const MODEL_ROUTE_PRESETS: ModelRoutePreset[] = [
  {
    id: 'gpt-official',
    shortLabel: 'GPT-5.4 官方',
    vendorBadge: 'OpenAI 官方',
    logicalModelId: 'gpt-5.4',
    upstreamModelId: 'gpt-5.4',
    routeKind: 'official',
    routeBadge: '上路 · 直连 OpenAI 官方',
    accentLight: '#006874',
    accentDark: '#36d9ef',
    waveHex: '#00a6b8',
    protocolLabel: '原生 Responses 接口（不经格式转换）',
    authAction: '使用你的 ChatGPT 账号直连',
    authDetail: '只发往 OpenAI 官方，日常从不改动 auth.json',
    egressEndpoint: 'chatgpt.com/backend-api/codex/responses',
    capabilities: ['原订阅额度', '官方联网搜索', '官方生图', '多档思考', '原速流式响应'],
    gimbalYawRad: -0.42,
    gimbalPitchRad: 0.06,
    vaultActive: false,
    summary:
      '选中官方模型时，本机路由直接把请求送往 OpenAI 上游（走上方直通轨道），完整保留 ChatGPT 登录状态、订阅额度、联网搜索和图像生成。',
  },
  {
    id: 'claude-openrouter',
    shortLabel: 'Claude Opus 4.6',
    vendorBadge: 'OpenRouter',
    logicalModelId: 'openrouter/claude-opus-4.6',
    upstreamModelId: 'anthropic/claude-opus-4.6',
    routeKind: 'bridge',
    routeBadge: '中路 · 云端接口转换',
    accentLight: '#7b4384',
    accentDark: '#eab0f3',
    waveHex: '#9c56a6',
    protocolLabel: 'Codex 指令 ⇄ Chat Completions 实时互译',
    authAction: '自动剥离 ChatGPT 令牌 · 换用钥匙串 API Key',
    authDetail: '仅从系统钥匙串读取 OpenRouter 密钥，绝不外泄官方登录信息',
    egressEndpoint: 'openrouter.ai/api/v1/chat/completions',
    capabilities: ['自动改写 apply_patch', '支持 MCP 工具', '图片截图识别', '思考强度映射'],
    gimbalYawRad: 0.0,
    gimbalPitchRad: 0.0,
    vaultActive: true,
    summary:
      '中间的转向台会把请求交给协议转换模块，把 Codex 专用指令与文件补丁工具译成标准 Chat 接口，同时从系统钥匙串单独读取该服务商的密钥。',
  },
  {
    id: 'qwen-newapi',
    shortLabel: 'Qwen4 Max',
    vendorBadge: 'NewAPI 聚合',
    logicalModelId: 'newapi/qwen4-max',
    upstreamModelId: 'qwen4-max',
    routeKind: 'bridge',
    routeBadge: '中路 · 云端接口转换',
    accentLight: '#835500',
    accentDark: '#feb958',
    waveHex: '#c48100',
    protocolLabel: 'Codex 指令 ⇄ 聚合网关兼容流',
    authAction: '自动剥离 ChatGPT 令牌 · 换用钥匙串 API Key',
    authDetail: '禁用重定向上游盗密钥风险；中文流式分包自动拼齐不断字',
    egressEndpoint: 'api.newapi.internal/v1/chat/completions',
    capabilities: ['中文不断字拼接', '工具兼容展平', '思考摘要流', '流式/整包自适应'],
    gimbalYawRad: 0.02,
    gimbalPitchRad: -0.04,
    vaultActive: true,
    summary:
      '针对 NewAPI / OneAPI 等团队或个人中转站做了专门适配：就算上游把中文切在数据包边缘或返回非流式整包，也能稳定还原给 Codex。',
  },
  {
    id: 'deepseek-oneapi',
    shortLabel: 'DeepSeek V4.1',
    vendorBadge: 'OneAPI',
    logicalModelId: 'oneapi/deepseek-v4.1',
    upstreamModelId: 'deepseek-v4.1',
    routeKind: 'bridge',
    routeBadge: '中路 · 深度思考转换',
    accentLight: '#365e91',
    accentDark: '#9ec5ff',
    waveHex: '#4876b5',
    protocolLabel: 'Responses ⇄ DeepSeek 思考流互译',
    authAction: '自动剥离 ChatGPT 令牌 · 换用钥匙串 API Key',
    authDetail: '清理不可跨模型复用的不透明字段，只传递可读对话记录',
    egressEndpoint: 'oneapi.team.example/v1/chat/completions',
    capabilities: ['实时展示思考过程', '长推理不断流', '工具参数流式拼装', '跨模型接力记忆'],
    gimbalYawRad: -0.02,
    gimbalPitchRad: 0.04,
    vaultActive: true,
    summary:
      '自动识别 DeepSeek 的推理输出字段，转成 Codex 原生思考摘要进度条；并针对长耗时推理保持稳健超时期限，深思考中途不断线。',
  },
  {
    id: 'gemini-newapi',
    shortLabel: 'Gemini 3.8 Flash',
    vendorBadge: 'NewAPI 多模态',
    logicalModelId: 'newapi/gemini-3.8-flash',
    upstreamModelId: 'gemini-3.8-flash',
    routeKind: 'bridge',
    routeBadge: '中路 · 多模态图文转换',
    accentLight: '#006a5c',
    accentDark: '#6ee0cc',
    waveHex: '#188f7e',
    protocolLabel: 'Responses ⇄ 多模态图文兼容流',
    authAction: '自动剥离 ChatGPT 令牌 · 换用钥匙串 API Key',
    authDetail: '工具返回的终端截图与文档图像会自动整理并附上，不丢画面',
    egressEndpoint: 'api.newapi.internal/v1/chat/completions',
    capabilities: ['百万长上下文', '读懂工具截图', '文档图片理解', '低延迟首字响应'],
    gimbalYawRad: 0.01,
    gimbalPitchRad: 0.02,
    vaultActive: true,
    summary:
      '很多第三方接口不支持在工具回执里直接带图。OmniBridge 会自动把截图和附件提升为紧随其后的多模态消息，让视觉模型看懂命令输出和页面截图。',
  },
  {
    id: 'ollama-local',
    shortLabel: 'Ollama · Qwen3 Coder',
    vendorBadge: '本机离线',
    logicalModelId: 'ollama/qwen3-coder',
    upstreamModelId: 'qwen3-coder:30b',
    routeKind: 'local',
    routeBadge: '下路 · 本机 127.0.0.1 离线运行',
    accentLight: '#2b6832',
    accentDark: '#8cd891',
    waveHex: '#35883f',
    protocolLabel: '本机直连 Responses ⇄ Ollama Chat 接口',
    authAction: '无认证免密发送 · 绝不联网',
    authDetail: '仅 127.0.0.1 本机地址允许免 HTTPS；不携带任何凭据或账号头',
    egressEndpoint: '127.0.0.1:11434/v1/chat/completions',
    capabilities: ['100% 离线不出网', '免密钥直连', '兼容 Ollama/vLLM/LM Studio', '零云端费用'],
    gimbalYawRad: 0.44,
    gimbalPitchRad: -0.05,
    vaultActive: false,
    summary:
      '选到本机模型时，请求走向下方绿色闭环轨道，全程留在本机 127.0.0.1 网卡里，适合内网保密代码审查、离线开发或低成本批量任务。',
  },
];

export interface HowItWorksStep {
  id: string;
  stepTag: string;
  crateTag: string;
  iconClass: string;
  title: string;
  lead: string;
  points: string[];
}

export const HOW_IT_WORKS_STEPS: HowItWorksStep[] = [
  {
    id: 'step-catalog',
    stepTag: '第一步 · 合进原生菜单',
    crateTag: 'codex-mp-catalog',
    iconClass: 'm3-i-hub',
    title: '不改动官方客户端，模型直接出现在 /model 里',
    lead: '从你安装的官方 codex 读取真实内建模型目录，把你开启的第三方和本机模型无损合进去。',
    points: [
      '保留官方模型所有原生开关，不因为 Codex 升级而丢失新字段。',
      '自动关闭第三方模型不支持的沙箱特性，改用标准文件编辑与命令工具。',
      '改写配置时采用带注释的备份事务，任何时候都能一键恢复修改前的原始状态。',
    ],
  },
  {
    id: 'step-router',
    stepTag: '第二步 · 按轮次智能分流',
    crateTag: 'codex-mp-router',
    iconClass: 'm3-i-router',
    title: '每发一句都重新判别去向，一条会话不用绑定死',
    lead: '在你的电脑本机（127.0.0.1:8787）开启轻量转发，仅在当前设备内可访问，外部设备连不到。',
    points: [
      '按你当前选的模型精准查表，选回 GPT 就原样走 OpenAI 官方，其他模型转走对应网关。',
      '官方请求放行原装授权头与白名单头，订阅额度、官方联网和生图能力完全不受影响。',
      '控制面板更新配置后后台路由热重载，写代码途中不必重开终端或重启桌面客户端。',
    ],
  },
  {
    id: 'step-bridge',
    stepTag: '第三步 · 接口与工具自动翻译',
    crateTag: 'codex-mp-protocol-bridge',
    iconClass: 'm3-i-swap-horiz',
    title: '让只支持 OpenAI 标准接口的模型也能动手写代码',
    lead: '把 Codex 专用的 Responses 协议与自由语法工具，实时译成通用的 Chat Completions 流式请求。',
    points: [
      '完整转换 apply_patch 等修改文件工具与 MCP 插件工具，返回后再精确还原给 Codex。',
      '内置轻量化跨模型会话记录，半路切模型时自动补齐前面聊过的内容，防止断片。',
      '逐字节守护中文与符号边界、自动提取工具返回的截图图像，兼容 DeepSeek 与多模态模型。',
    ],
  },
  {
    id: 'step-security',
    stepTag: '第四步 · 密钥隔离与随时还原',
    crateTag: 'codex-mp-credentials',
    iconClass: 'm3-i-verified-user',
    title: '钥匙串加密存放，日常绝不动你的官方 auth.json',
    lead: '所有第三方 API Key 存入系统原生凭据管理器，配置文件中不出现明文密钥。',
    points: [
      '日常安装、同步、转发对话和卸载从不改写 ~/.codex/auth.json，也绝不把官方令牌发往第三方。',
      '使用可选的“切换官方账号”功能时，切换前会自动备份 0600 权限历史快照，方便随时退回。',
      '非本机回环地址强制要求 HTTPS、禁用上游 307/308 跳转防窃取；远程面板未设口令拒绝公网监听。',
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
    commandOrPrompt: '先帮我看下这个仓库的鉴权模块结构，顺便上网查下最新规范要求。',
    modelId: 'gpt-5.4',
    presetId: 'gpt-official',
    badge: '第 1 轮 · OpenAI 官方直连',
    routeSummary: '命中官方模型列表：直接发往 OpenAI 官方，保留官方联网搜索与订阅额度。',
    headersSent: [
      'Authorization: <你的 ChatGPT 登录令牌>',
      'ChatGPT-Account-Id: acc_8f92...d1',
      '对话标识: thr_019a8c...4e',
    ],
    headersStripped: ['已去除本地转发内部令牌，不透露本机额外头信息'],
    assistantPreview:
      '已通过官方联网核对规范，并定位了鉴权入口与路由分流代码，核心结构一共分三层……',
    latencyTag: '官方直连 · 原生响应',
  },
  {
    turnIndex: 2,
    slashSwitch: '/model openrouter/claude-opus-4.6',
    commandOrPrompt: '不错，就在刚才的上下文里，直接帮我把边界单元测试用补丁写进去。',
    modelId: 'openrouter/claude-opus-4.6',
    presetId: 'claude-openrouter',
    badge: '第 2 轮 · 同对话换 Claude',
    routeSummary: '保持同一会话！自动带入第 1 轮对话内容，同时彻底清掉 ChatGPT 登录令牌并注入 OpenRouter 密钥。',
    headersSent: [
      'Authorization: <钥匙串读取 OpenRouter API Key>',
      'Content-Type: application/json',
    ],
    headersStripped: [
      'ChatGPT 登录令牌（已拦截，绝不发给第三方）',
      'ChatGPT-Account-Id（已拦截）',
      '不可跨平台使用的官方内部缓存字段（已清理）',
    ],
    assistantPreview:
      '已根据刚才的分析写出 3 组边界测试用例，并通过 apply_patch 工具直接写入文件。',
    latencyTag: '云端转换 · 工具兼容',
  },
  {
    turnIndex: 3,
    slashSwitch: '/model ollama/qwen3-coder',
    commandOrPrompt: '离线检查一下刚才这段测试代码里有没有写死任何真实密钥或内网地址。',
    modelId: 'ollama/qwen3-coder',
    presetId: 'ollama-local',
    badge: '第 3 轮 · 本机离线运行',
    routeSummary: '切换为本机 127.0.0.1:11434 运行 Ollama，数据不出本机，不带任何授权头。',
    headersSent: ['Content-Type: application/json（免密钥直连本机）'],
    headersStripped: [
      '所有账号或第三方 API 密钥头（已清除）',
      '零外网 DNS 与云端上传请求',
    ],
    assistantPreview:
      '已在本机离线检查完毕：测试代码只使用了模拟测试数据与 127.0.0.1 演示地址，没有任何真实凭据。',
    latencyTag: '本机 127.0.0.1 · 0 流量外发',
  },
  {
    turnIndex: 4,
    slashSwitch: '/model gpt-5.4',
    commandOrPrompt: '切回官方模型，帮我汇总今天这几轮改了什么，再画一张示意草图。',
    modelId: 'gpt-5.4',
    presetId: 'gpt-official',
    badge: '第 4 轮 · 无缝回到官方',
    routeSummary: '无需重启或重新登录，立刻切回 OpenAI 官方，前 3 轮记忆完整衔接，直接可用官方生图与额度。',
    headersSent: [
      'Authorization: <你的 ChatGPT 登录令牌>',
      'ChatGPT-Account-Id: acc_8f92...d1',
      '对话标识: thr_019a8c...4e',
    ],
    headersStripped: ['第三方服务商 API Key 绝不会发往 OpenAI 官方'],
    assistantPreview:
      '已无缝回到同一对话：前面 4 轮的来龙去脉全部在线，改动总结和架构草图已生成，随时可以继续。',
    latencyTag: '官方直连 · 状态完好',
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
    layer: '核心规则',
    responsibility: '服务商数据结构、HTTPS 地址强制校验、文件锁与受限权限写入',
    highlight: '拦截内网元数据地址与明文外网 HTTP',
  },
  {
    crate: 'codex-mp-catalog',
    layer: '菜单合并',
    responsibility: '读取官方自带模型定义，合理克隆模板生成新模型并兼容新字段',
    highlight: '官方 Codex 升级时不丢失未知属性',
  },
  {
    crate: 'codex-mp-credentials',
    layer: '钥匙串',
    responsibility: '统一调用 macOS 钥匙串、Windows 凭据管理器与 Linux Secret Service',
    highlight: '兼容 Windows 超长 OAuth 令牌安全回退',
  },
  {
    crate: 'codex-mp-protocol-bridge',
    layer: '协议翻译',
    responsibility: 'Responses 与 Chat Completions 互译、工具编码、图片提取与历史补全',
    highlight: '流式传输中文不断字，支持推理展示',
  },
  {
    crate: 'codex-mp-router',
    layer: '本机分流',
    responsibility: '在 127.0.0.1:8787 按轮次精准分流、禁止重定向、密钥代际热更新',
    highlight: '防 307/308 跳转窃取密钥，长思考不断流',
  },
  {
    crate: 'codex-mp-integration',
    layer: '安全配置',
    responsibility: '按保留注释的方式更新 ~/.codex/config.toml 并核对修改指纹',
    highlight: '发现用户手改冲突时不盲目覆盖，保障安全',
  },
  {
    crate: 'codex-mp-manager',
    layer: '进程守护',
    responsibility: '服务商模型自动拉取、本地路由守护与官方多账号安全切换备份',
    highlight: '切换账号前自动保留历史快照供随时退回',
  },
  {
    crate: 'codex-mp-web',
    layer: '网页控制端',
    responsibility: '单文件内置 Web 界面（默认仅本机 :31828）、口令哈希与限流',
    highlight: '未设访问口令时禁止对外开放局域网监听',
  },
  {
    crate: 'codex-mp-desktop',
    layer: '可选桌面启动',
    responsibility: '面向特殊场景提供独立的桌面启动包装与文件指纹检查',
    highlight: '支持一键恢复官方原始桌面程序入口',
  },
  {
    crate: 'codex-mp-cli',
    layer: '统一命令行',
    responsibility: '提供 setup / sync / launch / resume / doctor / web / tray 子命令',
    highlight: '纯原生单一可执行文件，无外部运行环境依赖',
  },
];

export interface ReleaseAssetLink {
  label: string;
  fileName: string;
  url: string;
  primary?: boolean;
}

export interface ReleasePlatformCard {
  id: string;
  osTag: string;
  title: string;
  desc: string;
  links: ReleaseAssetLink[];
}

const RELEASE_BASE = 'https://github.com/IGNGserver/codex-omnibridge/releases/download/v0.1.2';

export const GITHUB_REPO_URL = 'https://github.com/IGNGserver/codex-omnibridge';
export const GITHUB_RELEASES_URL = 'https://github.com/IGNGserver/codex-omnibridge/releases';
export const RELEASE_TAG_URL = 'https://github.com/IGNGserver/codex-omnibridge/releases/tag/v0.1.2';
export const RELEASE_SHA256_URL = `${RELEASE_BASE}/SHA256SUMS`;

export const RELEASE_PLATFORMS: ReleasePlatformCard[] = [
  {
    id: 'macos-arm',
    osTag: 'macOS · Apple Silicon',
    title: 'macOS (M1–M4)',
    desc: '原生支持苹果芯片与系统钥匙串，包含桌面应用与菜单栏常驻控制。',
    links: [
      {
        label: '下载 .dmg 安装包',
        fileName: 'codex-omnibridge-macos-aarch64-0.1.2.dmg',
        url: `${RELEASE_BASE}/codex-omnibridge-macos-aarch64-0.1.2.dmg`,
        primary: true,
      },
      {
        label: '.tar.gz 压缩包',
        fileName: 'codex-omnibridge-macos-aarch64-0.1.2.tar.gz',
        url: `${RELEASE_BASE}/codex-omnibridge-macos-aarch64-0.1.2.tar.gz`,
      },
    ],
  },
  {
    id: 'macos-x64',
    osTag: 'macOS · Intel x64',
    title: 'macOS (Intel)',
    desc: '原生 x86_64 芯片安装包，免转译全速运行，同样集成钥匙串与命令行。',
    links: [
      {
        label: '下载 .dmg 安装包',
        fileName: 'codex-omnibridge-macos-x86_64-0.1.2.dmg',
        url: `${RELEASE_BASE}/codex-omnibridge-macos-x86_64-0.1.2.dmg`,
        primary: true,
      },
      {
        label: '.tar.gz 压缩包',
        fileName: 'codex-omnibridge-macos-x86_64-0.1.2.tar.gz',
        url: `${RELEASE_BASE}/codex-omnibridge-macos-x86_64-0.1.2.tar.gz`,
      },
    ],
  },
  {
    id: 'windows-x64',
    osTag: 'Windows · 64 位',
    title: 'Windows x64',
    desc: '带完整中文安装与卸载向导，对接 Windows 凭据管理器并支持系统托盘。',
    links: [
      {
        label: '下载 Setup.exe 安装程序',
        fileName: 'codex-omnibridge-windows-x86_64-0.1.2-setup.exe',
        url: `${RELEASE_BASE}/codex-omnibridge-windows-x86_64-0.1.2-setup.exe`,
        primary: true,
      },
      {
        label: '.zip 免安装版',
        fileName: 'codex-omnibridge-windows-x86_64-0.1.2.zip',
        url: `${RELEASE_BASE}/codex-omnibridge-windows-x86_64-0.1.2.zip`,
      },
    ],
  },
  {
    id: 'linux-x64',
    osTag: 'Linux · 64 位',
    title: 'Linux x64',
    desc: '支持 Debian/Ubuntu 安装包、通用单文件 AppImage 及纯命令行压缩包。',
    links: [
      {
        label: '下载 .deb 软件包',
        fileName: 'codex-omnibridge_0.1.2_amd64.deb',
        url: `${RELEASE_BASE}/codex-omnibridge_0.1.2_amd64.deb`,
        primary: true,
      },
      {
        label: 'AppImage 单文件',
        fileName: 'Codex.OmniBridge-0.1.2.AppImage',
        url: `${RELEASE_BASE}/Codex.OmniBridge-0.1.2.AppImage`,
      },
      {
        label: '.tar.gz 压缩包',
        fileName: 'codex-omnibridge-linux-x86_64-0.1.2.tar.gz',
        url: `${RELEASE_BASE}/codex-omnibridge-linux-x86_64-0.1.2.tar.gz`,
      },
    ],
  },
];
