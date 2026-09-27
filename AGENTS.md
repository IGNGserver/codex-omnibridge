# codex模型切换器（codex-omnibridge）· 项目 Agent 规范
> 只写本仓库与设备级规范的差异。Git 纪律、worktree、冲突处理见 `~/.qoder/coder-rules/global-rules.md`。

Collaboration: solo
Default branch: main
Integration: direct-after-validation
Release: tag + Actions（`.github/workflows/` 2 个）
Worktree: `~/项目/.wt/codex模型切换器/<slug>`

## 这是什么
Rust CLI + Web 面板（`crates/`、`apps/panel`、`crates/web`），用于切换 Codex 的模型提供方。

## 验证命令
- Node 侧（package.json 实测存在）：`npm run check:panel`、`npm run verify:panel`；打包链路 `npm run pack`、`npm run dist`。
- Rust 侧：`cargo test`、`cargo clippy`。**这两条尚未在本仓库实测跑通**，首次执行失败要报告缺口，不得改用未登记命令或跳过。

## 项目特殊限制
- 工作区约 25G 是未跟踪产物（`target/`、`node_modules/`、`dist/`），而 `.git` 只有 931M、跟踪文件仅 124 个。产物**不得提交，也不得自动清理**；要释放空间先列清单交用户确认。
- 本仓库经手模型凭据与 provider 配置：真实 API key 只能留在未跟踪配置文件里，不得进入源码、文档、提交信息或日志。
- 历史提交作者邮箱曾被本地 `user.email` 错设为 `github-actions[bot]@users.noreply.github.com`（2026-09-27 已清除）。新提交走全局身份，Agent 参与写 `Assisted-by: <harness>/<model>` trailer，不得再出现机器人署名。

## 集成与发布
- 集成前 `git fetch origin --prune`；`origin/main` 前进时 `rebase origin/main` 并重跑验证。禁止 `git pull`、`git push --force`。
- 多 worktree 并行时 `CARGO_TARGET_DIR` 指向共享目录，禁止每个 worktree 各存一份 `target/`。
