# Changelog

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

> **发版前必做**：把 `[Unreleased]` 节段下的内容移到新的 `## [X.Y.Z] - YYYY-MM-DD` 节段。
> GitHub Actions 的 Release workflow 会按 tag 版本号从这里抽取对应节段，写入 `latest.json` 的 `notes` 字段。
> 如果对应版本节段不存在或为空，`notes` 会兜底为字符串 `"Version X.Y.Z"`。

## [Unreleased]

## [1.0.5] - 2026-04-28

### 新增

- 接入 `tauri-plugin-updater`，应用启动 30 秒后静默检查新版本，发现新版后弹窗提示「立即更新 / 稍后提醒 / 跳过此版本」。
- GitHub Actions 全平台打包流水线：macOS arm64 / Intel + Windows NSIS 三平台并行构建，自动生成签名与 `latest.json`。
- macOS DMG 直接打进 release artifact，下载后双击即可安装。

### 修复

- USB→WiFi 切换前增加网络可达性 preflight：电脑无网络或与手机不在同一网段时立即报错（<10ms），USB 保持连接，避免之前「USB 先断开再卡 30 秒」的体验。
- 过渡页面在 Windows 上卡片左对齐 + 无法拖动窗口的问题（`.transition-content` 加 `w-full`，根因 macOS 透明窗口掩盖了同样的 bug）。

### 内部

- 三处版本号建立 SSOT：`npm run version:bump <ver>` 一键同步 `package.json` / `Cargo.toml` / `tauri.conf.json`。
- adb sidecar 二进制纳入 git，CI 构建依赖完整。

## [1.0.4] - 2026-04-27

### 修复

- USB→WiFi ADB 切换的 ghost row 竞态：通过 eager upsert + ghost transport guard 避免设备短暂消失。
- 设备卡片 transport chip 替换为可点击的 `transportIconButton`，切换中保持 spinner 状态。
- WiFi 切换 polling 替代固定 2 秒 sleep，缩短正常路径耗时。

## [1.0.3] - 2026-04-21

### 修复

- 多平台打包脚本完善（macOS Intel/ARM64 + Windows）。

## [1.0.2] - 2026-04-20

- 早期版本，详见 git log。
