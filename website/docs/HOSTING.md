# 托管与部署（Cloudflare Pages）

> 正式站点：<https://omnibridge.igng.net/>。站点源码位于产品主仓库 `IGNGserver/codex-omnibridge` 的 `website/`，采用 Cloudflare Pages **Direct Upload** 托管，持续校验与部署工作流为 `.github/workflows/website.yml`。

## 当前配置

| 项 | 值 |
| --- | --- |
| Pages 项目 | `omnibridge-website`（Direct Upload，项目 ID `a86a442b-081d-443d-8a30-e8af5f79ab16`） |
| 部署来源 | `IGNGserver/codex-omnibridge` 的 `website/` |
| 部署工作流 | `.github/workflows/website.yml` |
| 生产分支 | `main` |
| 构建命令 | `pnpm install --frozen-lockfile --ignore-workspace && pnpm run build`（在 `website/` 下） |
| 发布目录 | `website/dist/` |
| 自定义域名 | `https://omnibridge.igng.net/`（zone `igng.net`，zone ID `d3f06d3e7ef8cccf99e93ce9c4ea4f44`） |
| 默认别名 | `https://omnibridge-website.pages.dev/`（保留） |
| DNS | proxied CNAME `omnibridge.igng.net` → `omnibridge-website.pages.dev`（record ID `35d56389aa404d236d60bf64c3b97c50`） |
| 首版基线回滚点 | `f0b5fe64-c650-41a6-bd18-533dc9ff22c0`（对应 `main` 提交 `663ab92`，2026-11-14 首版发布） |

`SITE_URL` 固定为 `https://omnibridge.igng.net/`，与 `website/index.html`（canonical、`og:url`、`og:image`、JSON-LD）、`website/public/robots.txt`、`website/public/sitemap.xml` 及 Pages 项目环境变量保持一致。`website/` 使用独立的 `package.json` 与 `pnpm-lock.yaml`，不并入产品根目录的构建工作区。

## 构建参数与缓存头

| 项 | 值 |
| --- | --- |
| 安装 | `pnpm install --frozen-lockfile --ignore-workspace`（Node.js ≥ 22，`packageManager: pnpm@10.33.0`） |
| 构建 | `pnpm run build`（`tsc --noEmit && vite build`） |
| 验证 | `pnpm run verify`（Playwright 无头 Chromium WebGL + 桌面/移动端响应式 + 元数据完整性校验） |
| 输出 | `dist/`（纯静态，含 `404.html`、`_headers`、`robots.txt`、`sitemap.xml`、`og/og.png`、`models/omnibridge-photonic-matrix.glb`） |
| 响应头 | `website/public/_headers`：`/assets/*` 与 `/fonts/*` 一年期 `immutable`；首页附加可变字体 `Link: preload`（供 Cloudflare 103 Early Hints 使用）；全站启用 `X-Content-Type-Options: nosniff`、`X-Frame-Options: DENY`、`Referrer-Policy`、`Permissions-Policy`、`Cross-Origin-Opener-Policy` |

本地构建与验证必须在 `${BUILD_SCRATCH_ROOT:-$HOME/.build}` 本机盘镜像目录内执行，禁止在项目卷内产生 `node_modules/` 或 `dist/`。

## 部署通道与自动发布

支持两条等价的 Direct Upload 受控通道（均复用同一项目 `omnibridge-website`，禁止改为 Git 集成或重复建项）：

1. **GitHub Actions 自动部署（`.github/workflows/website.yml`）**：
   - `pull_request`（改动涉及 `website/**` 或 `website.yml`）：执行 `pnpm run build` 与 Playwright `pnpm run verify`，不上传；
   - `push` 到 `main` / `repository_dispatch`（`release-published`）/ `workflow_dispatch`：校验通过后，若仓库 Secrets 已配置最小权限 `CLOUDFLARE_API_TOKEN`（仅需 `Cloudflare Pages:Edit`）与 `CLOUDFLARE_ACCOUNT_ID`，自动调用 `cloudflare/wrangler-action@v3` 执行 `pages deploy dist --project-name omnibridge-website --branch main`。凭据只存放在 GitHub Actions Secrets，绝不写入仓库或日志。
2. **经授权 Cloudflare API Direct Upload（无长期本地 Token 模式）**：
   - 本机不落盘长期 `CLOUDFLARE_API_TOKEN` 时，经授权 Cloudflare API 网关获取单项目 30 分钟短期上传令牌（`GET /accounts/{account}/pages/projects/omnibridge-website/upload-token`），按 Wrangler 同款规则（`blake3(base64(content) + ext)` 前 32 位十六进制）在本机 scratch 完成 `/pages/assets/check-missing` → `upload` → `upsert-hashes`，再提交 `POST /accounts/{account}/pages/projects/omnibridge-website/deployments`（multipart 携带 `manifest`、`branch=main`、`commit_hash`、`commit_message` 与 `_headers`）。

## 回滚与域名维护

- **快速回滚**：在 Cloudflare Pages 控制台 `omnibridge-website` → `Deployments` 选择上一成功生产部署点击 Rollback，或通过 Cloudflare API 调用 `POST /accounts/{account_id}/pages/projects/omnibridge-website/deployments/{deployment_id}/rollback`（首版验证通过基线为 `f0b5fe64-c650-41a6-bd18-533dc9ff22c0`）。
- **上线后核对项**：`/` 返回 `200`、未知路径返回 `404`、`/models/omnibridge-photonic-matrix.glb` 返回 `200`、`/assets/*` 返回 `immutable` 缓存头、无外部 CDN 请求。
- **域名变更**：修改 DNS 前记录原值（当前为 proxied CNAME `omnibridge.igng.net` → `omnibridge-website.pages.dev`，record ID `35d56389aa404d236d60bf64c3b97c50`），变更时须同步更新 Pages 自定义域名与全站 `SITE_URL`。

## 搜索收录与隐私边界

- **Google Search Console**：父域资源 `sc-domain:igng.net` 已覆盖本子域；`public/robots.txt` 的 `Sitemap:` 指向静态 `public/sitemap.xml`。
- **Bing Webmaster Tools**：首页已内置账号级验证标签 `<meta name="msvalidate.01" content="B411826ACAE5C5CD20EA5574C3F8A6E4">`，与其他 `*.igng.net` 官方站点保持一致，请勿删除。
- **零追踪承诺**：全站无第三方统计脚本、无 Cookie、无外部字体或模型 CDN 请求。

## 历史记录

- **2026-11-14**：首版 3D 光学交换路由矩阵与 M3 Expressive 控制中心官网合入主仓库 `website/`（PR #2，`663ab92`），创建 Cloudflare Pages Direct Upload 项目 `omnibridge-website`，绑定正式域名 `https://omnibridge.igng.net/` 并完成生产上线。
