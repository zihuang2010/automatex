# 构建与部署

## 1. 开发环境搭建

### 前置依赖

| 工具    | 版本                  | 用途     |
| ------- | --------------------- | -------- |
| Node.js | ≥ 18                  | 前端构建 |
| Rust    | stable (2021 edition) | 后端编译 |
| ADB     | 内嵌 sidecar          | 设备通信 |

### 初始安装

```bash
# 安装前端依赖
npm install

# 验证 Rust 工具链
rustup show
```

### 开发模式

```bash
# 同时启动 Vite 开发服务器 + Tauri 客户端
npm run tauri dev
```

- Vite 监听 `localhost:1420`
- 支持 HMR 热更新
- 自动忽略 `backends/` 目录变更

## 2. 构建流程

### macOS 构建

```bash
# 直接构建 macOS 应用
npm run tauri build

# 或使用构建脚本
bash scripts/build-macos.sh
```

产物路径: `backends/target/release/bundle/`

### Windows 交叉编译 (从 macOS)

```bash
bash scripts/build-windows.sh
```

**脚本自动处理**：

1. ✅ 安装 `cargo-xwin`
2. ✅ 配置 Windows MSVC 工具链
3. ✅ 下载 xwin SDK 头文件
4. ✅ 设置环境变量 (`CC_x86_64_pc_windows_msvc`, `CXX_x86_64_pc_windows_msvc`)
5. ✅ 执行 `cargo xwin build --release --target x86_64-pc-windows-msvc`
6. ✅ 收集产物到 `backends/target/x86_64-pc-windows/`

### 前端单独构建

```bash
# TypeScript 编译 + Vite 打包
npm run build
```

### Rust 单独编译检查

```bash
cd backends && cargo check
```

## 3. 代码质量

### Linting

```bash
# ESLint 检查
npm run lint

# Prettier 格式化
npm run format

# Prettier 检查（不修改）
npm run format:check
```

### Pre-commit Hooks

通过 Husky + lint-staged 配置，提交前自动运行：

- `.ts/.tsx`: ESLint --fix + Prettier
- `.css/.html/.json/.md`: Prettier

## 4. Vite 配置要点

```typescript
export default defineConfig({
    plugins: [tailwindcss()], // Tailwind CSS v4 Vite 插件
    clearScreen: false, // 不清屏（便于查看 Rust 编译错误）
    server: {
        port: 1420, // Tauri 要求固定端口
        strictPort: true,
        watch: {
            ignored: ['**/backends/**'], // 忽略后端目录
        },
    },
});
```

## 5. Tauri 配置 (`tauri.conf.json`)

```json
{
    "app": {
        "withGlobalTauri": true,
        "macOSPrivateApi": true, // macOS 透明窗口需要
        "windows": [
            {
                "decorations": false, // 全平台无原生标题栏
                "transparent": true, // macOS 圆角窗口需要
                "backgroundColor": "#050202" // 开屏渐变边缘色
            }
        ]
    },
    "bundle": {
        "externalBin": ["binaries/adb"] // 内嵌 ADB sidecar
    }
}
```

## 6. 权限声明 (`capabilities/default.json`)

```json
{
    "permissions": [
        "core:default",
        "opener:default",
        "core:window:allow-close",
        "core:window:allow-minimize",
        "core:window:allow-toggle-maximize",
        "core:window:allow-start-dragging",
        "core:window:allow-is-maximized",
        "core:window:allow-set-background-color",
        "os:default"
    ]
}
```

## 7. 部署清单

### macOS

- 产物: `.app` 或 `.dmg`
- 路径: `backends/target/release/bundle/macos/`
- 签名: 需要 Apple Developer 证书（可选）

### Windows

- 产物: `AutomateX.exe` + `adb.exe`
- 路径: `backends/target/x86_64-pc-windows/`
- 部署: 整个文件夹复制到 Windows 机器
- 注意: `adb.exe` 必须与主程序在同一目录
