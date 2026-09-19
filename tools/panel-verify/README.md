# 面板验证套件

对 `apps/panel/` 做**真实浏览器渲染**验证（Chromium / Playwright），不是静态分析。
三个套件分别覆盖结构、对比度与功能，全部基于同一个 mock 后端运行。

## 为什么需要它

面板的多数缺陷在静态阅读时看不出来，在"只看当前主题的截图"里也看不出来：

- 两层低 alpha 叠加（12% 容器 + 38% 文字）把标签压到 **1.00:1** 对比度；
- 浮动标签缺 `for`/`id` 时，字段在可访问性树中**完全没有名称**；
- 容器查询写在网格自身上时（容器不能查询自己），所有网格永远停在 1 列；
- `9999px` 起点的圆角过渡会被夹到盒子高度，形态变形**完全不可见**。

这些都由本套件捕获。

## 运行

Playwright **不是本项目的依赖**（面板本身零运行时依赖），按需解析。

```bash
# CI：安装后运行
npm i -D playwright && npx playwright install --with-deps chromium
node tools/panel-verify/run.mjs

# 本地：指向已有的安装
PLAYWRIGHT_PATH=/path/to/playwright \
CHROME_PATH=/path/to/chrome \
  node tools/panel-verify/run.mjs
```

也可单独运行某个套件（需先另起 mock 服务）：

```bash
node tools/panel-verify/serve.mjs &
node tools/panel-verify/contrast.mjs
```

## 套件

| 文件 | 覆盖 |
|---|---|
| `serve.mjs` | mock 后端 + 静态服务；镜像 Rust 后端的 CSP 与 404 语义 |
| `structure.mjs` | 5 档窗口尺寸 × 4 视图、6 个对话框、主题/动效矩阵、键盘与可访问名称 |
| `contrast.mjs` | 实际渲染的 WCAG AA 对比度，两主题 × 4 视图 |
| `functional.mjs` | 32 项用户操作，断言发出的 API 调用与界面结果 |

## 为什么针对 mock 后端而不是真实后端

套件默认跑在 `serve.mjs`（mock）上，这是刻意的。

真实后端在**未开启网页访问且未设密码**时只接受 loopback desktop token，而面板按安全
设计会在加载后立即把 `local_token` 从地址栏抹掉（避免经历史记录/Referrer 泄漏）。
于是套件里的多次导航与 reload 会丢掉这个内存中的 token，后续请求返回 403 —— 这是
安全模型**正确工作**，不是缺陷，但它使"反复导航"类断言无法在真实后端上稳定运行。

分工如下：

| 目标 | 方式 |
|---|---|
| 结构 / 对比度 / 功能（需多次导航） | mock 后端（本套件） |
| 真实后端资源、MIME、CSP、`file://` 链路 | 单次加载端到端检查 |

真实后端的单次加载验证可直接执行：

```bash
./target/debug/codex-mp web start --port 4620 --headless --local-token t
# 浏览器打开 http://127.0.0.1:4620/?local_token=t
# 或断言资源与响应头：
curl -sSI http://127.0.0.1:4620/ | grep -i content-security-policy
curl -o /dev/null -w '%{http_code}\n' http://127.0.0.1:4620/fonts/noto-sans-sc.woff2
curl -o /dev/null -w '%{http_code}\n' http://127.0.0.1:4620/missing.woff2   # 期望 404
```

## 环境变量

| 变量 | 作用 |
|---|---|
| `PANEL_BASE` | 被验证面板的地址（默认 `http://127.0.0.1:4599`） |
| `PANEL_PORT` | mock 服务端口（默认 `4599`） |
| `PANEL_SHOTS` | 截图输出目录（默认 `panel-verify-shots`） |
| `PLAYWRIGHT_PATH` | Playwright 安装路径 |
| `CHROME_PATH` | 指定的 Chromium 可执行文件 |

截图目录已加入 `.gitignore`。
