# Release 发布流程

发布入口是 GitHub Actions 的 `Release` workflow，默认用于发布 prerelease。

## 发布规则

1. `release_channel` 默认是 `prerelease`，例如 `v0.1.0-rc.1`；不需要额外确认。
2. 只有明确要发布正式版时才选择 `release`，并填写精确的确认值
   `PUBLISH_FORMAL_RELEASE`，且版本号不能带 `-rc.1` 之类的 prerelease 后缀；缺少确认值时
   workflow 会在构建前失败。
3. `release_notes` 必填，内容会直接作为 GitHub Release 的正文；空白说明会被拒绝。
4. 版本号必须是 semver，可带 `v` 前缀。打包器会去掉 `v`，避免 Debian 版本字段非法。
5. 已存在的 Git tag 或 GitHub Release 不会被覆盖，需选择新的版本号。

## 必须上传的安装包

发布 job 会在创建 Release 前检查附件矩阵：

- Windows：原生 Inno Setup `*-setup.exe`，另附 portable `.zip`；
- macOS：Intel `x86_64` 和 Apple Silicon `aarch64` 两个 `.dmg`，另附 portable `.tar.gz`；
- Linux：`.deb` 和 x86_64 `.AppImage`，另附 portable `.tar.gz`；
- `SHA256SUMS`：由发布 job 对全部附件生成。

任意必需安装包缺失、重名或更新说明为空，Release 都不会创建。

## 本地打包检查

```bash
cargo fmt --all -- --check
cargo test --workspace --locked
cargo build --release --locked --package codex-mp-cli
bash -n scripts/build-deb.sh scripts/build-appimage.sh scripts/build-dmg.sh
./scripts/build-deb.sh v0.1.0-rc.1 amd64
```

AppImage 需要本机安装 `appimagetool`，macOS DMG 需要在对应 macOS runner 上由
`hdiutil` 生成；Windows Setup.exe 由 CI 安装 Inno Setup 后生成。
