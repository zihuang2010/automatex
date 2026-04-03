# 构建与打包

## 1. 当前打包策略

AutomateX 现在采用“编译期内嵌 + 运行时自动释放”方案：

- `adb`
- `scrcpy-server`
- Windows 的 `AdbWinApi.dll`
- Windows 的 `AdbWinUsbApi.dll`

都会在构建时嵌入主程序。应用首次启动时，会自动将这些文件释放到应用私有目录，再从那里调用。

这意味着：

- macOS：发布给用户的是一个 `.app` / `.dmg`
- Windows：发布给用户的是一个安装器 `.exe`，或一个单独的主程序 `.exe`

对最终用户来说，不再需要额外携带 `adb.exe`、`scrcpy-server` 等外部文件。

## 2. 源资源准备

构建时仍然需要这些源文件存在于仓库中：

| 路径                                                                                                                                             | 用途                       |
| ------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------- |
| [backends/resources/scrcpy-server](/Users/pis0sion/Pis0sion/RustCode/automatex/backends/resources/scrcpy-server)                                 | 设备端 scrcpy server       |
| [backends/binaries/adb-aarch64-apple-darwin](/Users/pis0sion/Pis0sion/RustCode/automatex/backends/binaries/adb-aarch64-apple-darwin)             | Apple Silicon macOS 的 adb |
| [backends/binaries/adb-x86_64-apple-darwin](/Users/pis0sion/Pis0sion/RustCode/automatex/backends/binaries/adb-x86_64-apple-darwin)               | Intel macOS 的 adb         |
| [backends/binaries/adb-x86_64-pc-windows-msvc.exe](/Users/pis0sion/Pis0sion/RustCode/automatex/backends/binaries/adb-x86_64-pc-windows-msvc.exe) | Windows x64 的 adb         |

Windows 如果要做到最稳，建议额外提供：

- [backends/resources/windows/AdbWinApi.dll](/Users/pis0sion/Pis0sion/RustCode/automatex/backends/resources/windows/AdbWinApi.dll)
- [backends/resources/windows/AdbWinUsbApi.dll](/Users/pis0sion/Pis0sion/RustCode/automatex/backends/resources/windows/AdbWinUsbApi.dll)

## 3. 打包前检查

```bash
npm run package:doctor
```

指定目标检查：

```bash
npm run package:doctor -- aarch64-apple-darwin
npm run package:doctor -- x86_64-pc-windows-msvc
```

它会检查：

- 对应 target 的 `adb` 源文件
- `scrcpy-server`
- Windows DLL 是否已提供

## 4. 开发模式

```bash
npm install
npm run tauri dev
```

## 5. macOS 打包

```bash
npm run package:mac
```

等价于：

```bash
bash scripts/build-macos.sh
```

产物目录：

- `backends/target/output/macos-arm64`
- `backends/target/output/macos-x64`

## 6. Windows 构建

从 macOS 交叉构建：

```bash
npm run package:win
```

等价于：

```bash
bash scripts/build-windows.sh
```

脚本会生成一个单独的主程序 `.exe`，运行时自动释放 `adb`、`scrcpy-server` 和可选 DLL。

产物目录：

- `backends/target/output/windows-x64-single`

如果你要最终给用户分发 Windows 安装器，建议在原生 Windows 机器上执行：

```bash
npm install
npm run build
npx tauri build --target x86_64-pc-windows-msvc
```

## 7. 运行时行为

应用启动时会把内嵌资源释放到应用私有目录：

- macOS：`App Local Data/runtime-sidecars/`
- Windows：`App Local Data\\runtime-sidecars\\`

随后所有 ADB / scrcpy 调用都走这个运行时目录，不再依赖应用安装目录旁边存在 sidecar 文件。

## 8. 常见问题

### 打包后提示找不到 `scrcpy-server`

先检查构建源文件是否存在：

- [backends/resources/scrcpy-server](/Users/pis0sion/Pis0sion/RustCode/automatex/backends/resources/scrcpy-server)

再重新执行：

```bash
npm run package:mac
```

### Windows 上 `adb.exe` 无法启动

优先补齐：

- `AdbWinApi.dll`
- `AdbWinUsbApi.dll`

然后重新构建 Windows 目标。

### 为什么现在可以做成单文件？

因为 `adb` 和 `scrcpy-server` 不再依赖 Tauri 的 `externalBin/resources` 在安装目录旁边存在，而是在编译时直接嵌入主程序，再由应用在首次启动时自动释放。这样分发时只需要给用户一个应用即可。
