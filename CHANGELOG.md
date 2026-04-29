# Changelog

格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

> **发版前必做**：把 `[Unreleased]` 节段下的内容移到新的 `## [X.Y.Z] - YYYY-MM-DD` 节段。
> GitHub Actions 的 Release workflow 会按 tag 版本号从这里抽取对应节段，写入 `latest.json` 的 `notes` 字段。
> 如果对应版本节段不存在或为空，`notes` 会兜底为字符串 `"Version X.Y.Z"`。

## [Unreleased]

## [1.0.6] - 2026-04-28

### 修复

- USB→WiFi 切换在 adb server 缓存了 endpoint 为 unreachable 状态时，仍会立即报 `No route to host` 的 bug：在 connect 重试前主动 `adb disconnect` 一次清掉 transport tracker 缓存，让重试真正去做 TCP 探测，不再读 stale 状态。这是慢网络下切换失败的最根本原因（adb daemon 一次失败就缓存，之后所有 connect 25ms 内立即返回失败，根本不真做 TCP）。
- USB→WiFi 切换 connect 重试预算从 2.5s 提升到 9s：新增 1.5s 初始等待让手机 adbd 先切到 TCP 监听 + WiFi 网卡 ARP 上线，重试间隔从 500ms 调到 1500ms。
- USB→WiFi 切换在多 ADB server 共存环境下（电脑同时跑 Android Studio 等占用 5037 端口的工具）报 `No route to host` 的 bug：`run_adb_async_raw` 现在跟同步版 `adb_command()` 行为一致，非默认端口自动注入 `-P <port>`，确保 `adb connect` / `disconnect` 命令打到 app 自己的 adb server 而不是别人家的。

### 内部

- USB→WiFi 切换流程加全链路 `INFO` 级日志：`[switch_to_wifi]` / `[connect_wifi]` / `[run_adb_async_raw]`，包含 adb path / port / args / exit / 耗时 / stdout / stderr，方便后续诊断。

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
