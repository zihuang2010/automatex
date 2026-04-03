# Windows ADB Runtime Dependencies

将 Windows 平台的 ADB 运行时依赖放在这个目录：

- `AdbWinApi.dll`
- `AdbWinUsbApi.dll`

打包后的应用启动时会尝试将这两个 DLL 同步到 `adb.exe` 所在目录。

推荐来源：

- Android Platform Tools 官方发行包

注意：

- 这两个 DLL 仅 Windows 需要
- macOS 打包不依赖本目录
