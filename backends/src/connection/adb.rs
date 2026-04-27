//! ADB 底层辅助函数
//!
//! 负责 adb 路径发现、命令构建、超时执行等底层操作。
//! DeviceManager 方法通过本模块与 ADB 交互。

use base64::Engine;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::OnceLock;
use tauri::{path::BaseDirectory, AppHandle, Manager};
use tracing::{debug, error, info, warn};

include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

const ADB_KEYBOARD_IME: &str = "com.android.adbkeyboard/.AdbIME";
const ADB_KEYBOARD_SWITCH_DELAY_MS: u64 = 120;
const ADB_KEYBOARD_RESTORE_DELAY_MS: u64 = 40;
static EMBEDDED_ADB_PATH: OnceLock<String> = OnceLock::new();
static EMBEDDED_SCRCPY_SERVER_PATH: OnceLock<String> = OnceLock::new();
static ADB_FALLBACK_PATH: OnceLock<String> = OnceLock::new();

/// 获取 adb 的运行时路径。
///
/// 优先使用启动时从主程序中释放到应用私有目录的 adb，
/// 其次回退到同目录 sidecar，再次回退到系统 PATH。
pub fn adb_path() -> &'static str {
    if let Some(path) = EMBEDDED_ADB_PATH.get() {
        return path.as_str();
    }

    ADB_FALLBACK_PATH.get_or_init(|| {
        if let Some(sidecar) =
            sidecar_dir().map(
                |dir| {
                    if cfg!(windows) {
                        dir.join("adb.exe")
                    } else {
                        dir.join("adb")
                    }
                },
            )
        {
            if sidecar.exists() {
                return sidecar.to_string_lossy().to_string();
            }
        }
        "adb".to_string()
    })
}

fn sidecar_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
}

fn resolve_resource_path_candidates(app: &AppHandle, candidates: &[&str]) -> Option<PathBuf> {
    candidates.iter().find_map(|relative| {
        app.path()
            .resolve(relative, BaseDirectory::Resource)
            .ok()
            .filter(|path| path.exists())
    })
}

fn embedded_runtime_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("获取应用本地数据目录失败: {}", e))?;
    Ok(base.join("runtime-sidecars"))
}

fn write_embedded_asset(
    target: &std::path::Path,
    bytes: &[u8],
    #[cfg_attr(not(unix), allow(unused_variables))] executable: bool,
) -> Result<(), String> {
    // LOG-1 修复：先毒大小（快路），大小相同时再全量内容比对，
    // 防止攻击者用相同大小的恶意二进制替换 adb/scrcpy-server。
    // 启动期读取一次（最多 8MB）可接受（<100ms on SSD）。
    let needs_write = match std::fs::metadata(target) {
        Ok(meta) if meta.len() as usize == bytes.len() => {
            // 大小相同：全量内容比对确保完整性
            std::fs::read(target).map(|existing| existing != bytes).unwrap_or(true)
        },
        Ok(_) => true,  // 大小不同，必须重写
        Err(_) => true, // 文件不存在
    };
    if needs_write {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建运行时资源目录失败 {}: {}", parent.display(), e))?;
        }
        std::fs::write(target, bytes)
            .map_err(|e| format!("写入运行时资源失败 {}: {}", target.display(), e))?;
    }

    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(target)
            .map_err(|e| format!("读取权限失败 {}: {}", target.display(), e))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(target, perms)
            .map_err(|e| format!("设置可执行权限失败 {}: {}", target.display(), e))?;
    }

    Ok(())
}

pub fn resolve_scrcpy_server_path(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(path) = EMBEDDED_SCRCPY_SERVER_PATH.get() {
        return Ok(PathBuf::from(path));
    }

    if let Some(path) =
        resolve_resource_path_candidates(app, &["resources/scrcpy-server", "scrcpy-server"])
    {
        return Ok(path);
    }

    if let Some(dir) = sidecar_dir() {
        let sibling = dir.join("scrcpy-server");
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    Err("找不到 scrcpy-server 资源，请确认已将 backends/resources/scrcpy-server 打包进应用".into())
}

pub fn prepare_packaged_sidecars(app: &AppHandle) {
    let runtime_dir = match embedded_runtime_dir(app) {
        Ok(dir) => dir,
        Err(err) => {
            error!(reason = %err, "初始化运行时资源目录失败");
            return;
        },
    };

    if let Some(bytes) = EMBEDDED_ADB_BYTES {
        let target = runtime_dir.join(if cfg!(windows) { "adb.exe" } else { "adb" });
        match write_embedded_asset(&target, bytes, true) {
            Ok(()) => {
                let _ = EMBEDDED_ADB_PATH.set(target.to_string_lossy().to_string());
                info!(path = %target.display(), "✓ 已准备内嵌 adb");
            },
            Err(err) => error!(reason = %err, "准备 adb 失败"),
        }
    } else {
        warn!("当前目标未内嵌 adb，将继续尝试 sidecar / PATH");
    }

    if let Some(bytes) = EMBEDDED_SCRCPY_SERVER_BYTES {
        let target = runtime_dir.join("scrcpy-server");
        match write_embedded_asset(&target, bytes, false) {
            Ok(()) => {
                let _ = EMBEDDED_SCRCPY_SERVER_PATH.set(target.to_string_lossy().to_string());
                info!(path = %target.display(), "✓ 已准备内嵌 scrcpy-server");
            },
            Err(err) => error!(reason = %err, "准备 scrcpy-server 失败"),
        }
    } else {
        warn!("当前目标未内嵌 scrcpy-server");
    }

    #[cfg(windows)]
    for (dll, bytes) in [
        ("AdbWinApi.dll", EMBEDDED_ADB_WIN_API_BYTES),
        ("AdbWinUsbApi.dll", EMBEDDED_ADB_WIN_USB_BYTES),
    ] {
        let Some(bytes) = bytes else {
            warn!(dll = dll, "Windows 运行时依赖未内嵌");
            continue;
        };
        let dst = runtime_dir.join(dll);
        match write_embedded_asset(&dst, bytes, false) {
            Ok(()) => info!(dll = dll, "✓ 已准备 Windows ADB 依赖"),
            Err(err) => error!(dll = dll, reason = %err, "同步 Windows ADB 依赖失败"),
        }
    }
}

/// 全局 ADB server 端口（默认 5037，启动时可自动调整）
static ADB_PORT: AtomicU16 = AtomicU16::new(5037);

/// 获取当前 ADB server 端口
pub fn adb_port() -> u16 {
    ADB_PORT.load(Ordering::Relaxed)
}

/// 启动时探测可用的 ADB server 端口
///
/// 尝试 5037-5047，找到正在运行 ADB server 或可用的端口。
/// 如果 5037 已被非 ADB 进程占用，自动切换到下一个可用端口。
pub fn resolve_adb_port() {
    let default_port: u16 = 5037;
    let max_port: u16 = 5047;

    for port in default_port..=max_port {
        // 尝试连接该端口，看是否已有 ADB server
        match std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            std::time::Duration::from_millis(300),
        ) {
            Ok(mut stream) => {
                // 端口有服务在监听，尝试 ADB 握手验证是否是 ADB server
                use std::io::Write;
                // ADB 协议: 发送 "host:version" 查询
                let msg = b"000Chost:version";
                if stream.write_all(msg).is_ok() {
                    use std::io::Read;
                    let mut buf = [0u8; 4];
                    stream.set_read_timeout(Some(std::time::Duration::from_millis(500))).ok();
                    if let Ok(n) = stream.read(&mut buf) {
                        if n == 4 && &buf == b"OKAY" {
                            // 确认是 ADB server
                            ADB_PORT.store(port, Ordering::Relaxed);
                            if port != default_port {
                                info!(
                                    port = port,
                                    default_port = default_port,
                                    "使用已有 ADB server (默认端口不可用)"
                                );
                            } else {
                                info!(port = port, "ADB server 已在运行");
                            }
                            return;
                        }
                    }
                }
                // 端口被非 ADB 进程占用，跳过
                warn!(port = port, "端口被非 ADB 进程占用，尝试下一个");
                continue;
            },
            Err(_) => {
                // 端口空闲，尝试在此端口启动 ADB server
                let result = run_adb_timed(
                    adb_command_raw().args(["-P", &port.to_string(), "start-server"]),
                    10,
                );
                match result {
                    Ok(output) if output.status.success() => {
                        ADB_PORT.store(port, Ordering::Relaxed);
                        if port != default_port {
                            info!(
                                port = port,
                                default_port = default_port,
                                "ADB server 已启动 (默认端口不可用)"
                            );
                        } else {
                            info!(port = port, "ADB server 已启动");
                        }
                        return;
                    },
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        error!(port = port, reason = %stderr.trim(), "端口启动失败");
                    },
                    Err(e) => {
                        error!(port = port, reason = %e, "端口启动失败");
                    },
                }
            },
        }
    }

    // 所有端口均不可用，保持默认值并打印警告
    error!(
        range_start = default_port,
        range_end = max_port,
        fallback_port = default_port,
        "所有端口均不可用，使用默认端口（可能无法正常工作）"
    );
}

/// 二进制完整性校验：验证 sidecar 文件存在且大小合理
///
/// 检测点：文件存在、大小 > 100KB（防截断）、可读。
/// Windows 额外检查 AdbWinApi.dll 和 AdbWinUsbApi.dll。
/// 在启动时调用一次即可，结果缓存在日志中。
pub fn verify_sidecar_integrity(app: Option<&AppHandle>) {
    let adb_name = if cfg!(windows) { "adb.exe" } else { "adb" };
    let adb_path = EMBEDDED_ADB_PATH
        .get()
        .map(PathBuf::from)
        .or_else(|| sidecar_dir().map(|dir| dir.join(adb_name)));
    match adb_path.and_then(|path| std::fs::metadata(&path).ok().map(|meta| (path, meta))) {
        Some((path, meta)) if meta.len() >= 100_000 => {
            info!(file = adb_name, size = meta.len(), path = %path.display(), "✓ 完整性检查通过");
        },
        Some((path, meta)) => {
            warn!(
                file = adb_name,
                size = meta.len(),
                path = %path.display(),
                "文件异常: 大小过小 (可能被截断/替换)"
            );
        },
        None => {
            warn!(file = adb_name, "未找到（内嵌运行时 / sidecar / PATH）");
        },
    }

    let scrcpy_resource = EMBEDDED_SCRCPY_SERVER_PATH.get().map(PathBuf::from).or_else(|| {
        app.and_then(|app| {
            resolve_resource_path_candidates(app, &["resources/scrcpy-server", "scrcpy-server"])
        })
    });
    match scrcpy_resource
        .or_else(|| {
            sidecar_dir().and_then(|dir| {
                let sibling = dir.join("scrcpy-server");
                sibling.exists().then_some(sibling)
            })
        })
        .and_then(|path| std::fs::metadata(&path).ok().map(|meta| (path, meta)))
    {
        Some((path, meta)) if meta.len() >= 50_000 => {
            info!(file = "scrcpy-server", size = meta.len(), path = %path.display(), "✓ 完整性检查通过");
        },
        Some((path, meta)) => {
            warn!(
                file = "scrcpy-server",
                size = meta.len(),
                path = %path.display(),
                "文件异常: 大小过小"
            );
        },
        None => {
            warn!(file = "scrcpy-server", "未找到（资源目录或 sidecar 同目录）");
        },
    }

    // Windows: 检查 ADB 运行时 DLL 依赖
    #[cfg(windows)]
    {
        let dlls = ["AdbWinApi.dll", "AdbWinUsbApi.dll"];
        for dll in &dlls {
            let path = EMBEDDED_ADB_PATH
                .get()
                .and_then(|adb| PathBuf::from(adb).parent().map(|dir| dir.join(dll)))
                .or_else(|| sidecar_dir().map(|dir| dir.join(dll)));
            if let Some(path) = path {
                if path.exists() {
                    info!(dll = *dll, "✓ Windows DLL 完整性检查通过");
                } else {
                    warn!(dll = *dll, path = ?path, "未找到 — adb.exe 可能无法正常运行");
                }
            }
        }
    }
}

/// 内部：创建不带 -P 参数的原始 ADB Command（仅用于 start-server 等引导命令）
#[allow(unused_mut)]
fn adb_command_raw() -> std::process::Command {
    let mut cmd = std::process::Command::new(adb_path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd
}

/// 创建不弹出控制台窗口的 ADB Command（自动带 -P 端口参数）
#[allow(unused_mut)]
pub fn adb_command() -> std::process::Command {
    let mut cmd = std::process::Command::new(adb_path());
    let port = adb_port();
    // 非默认端口时显式传递 -P 参数
    if port != 5037 {
        cmd.args(["-P", &port.to_string()]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd
}

/// 解析 WiFi 地址为 SocketAddr
pub(crate) fn parse_wifi_address(addr: &str) -> Result<std::net::SocketAddr, String> {
    let full = if addr.contains(':') { addr.to_string() } else { format!("{}:5555", addr) };
    let socket_addr = full
        .parse::<std::net::SocketAddr>()
        .map_err(|e| format!("地址格式错误 '{}': {}", addr, e))?;
    if socket_addr.port() == 0 {
        return Err(format!("端口号不能为 0: '{}'", addr));
    }
    Ok(socket_addr)
}

/// FIX #4: 带超时的 ADB 命令执行（防止进程永久阻塞）
/// 注意：此为同步版本，仅在 spawn_blocking 中使用
pub fn run_adb_timed(
    cmd: &mut std::process::Command,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("执行 adb 失败 (确保 adb 已安装): {}", e))?;

    let timeout = std::time::Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child.wait_with_output().map_err(|e| format!("读取输出失败: {}", e));
            },
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    // P0 修复：回收僵尸进程，防止进程句柄泄漏
                    let _ = child.wait();
                    return Err(format!("ADB 命令超时 ({}s)", timeout_secs));
                }
                // MEM-1 修复：躺询间隔 100ms → 20ms，
                // 降低 spawn_blocking 线程池最坏阻塞时间。
                // 并行 fetch 趄多时，OS 级 thread sleep 对内核准确无开销。
                std::thread::sleep(std::time::Duration::from_millis(20));
            },
            Err(e) => return Err(format!("等待命令失败: {}", e)),
        }
    }
}

/// P1 优化：异步 ADB 命令执行 — 零阻塞，内核事件驱动
///
/// 使用 `tokio::process::Command`，无需 `spawn_blocking`，无 100ms 轮询。
/// 适用于异步上下文（如 Tokio task 中直接 `.await`）。
#[allow(dead_code)]
pub async fn run_adb_async(
    serial: &str,
    args: &[&str],
    timeout_secs: u64,
) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(adb_path());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }

    cmd.args(["-s", serial])
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("spawn adb 失败: {}", e))?;

    // P1 修复：使用 wait() 而非 wait_with_output()（后者消费 self，超时后无法 kill）
    // 先取出 stdout/stderr handle，再 wait
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait()).await {
        Ok(Ok(status)) => {
            // 读取已管道化的 stdout/stderr
            let mut stdout_buf = Vec::new();
            if let Some(ref mut out) = stdout {
                use tokio::io::AsyncReadExt;
                let _ = out.read_to_end(&mut stdout_buf).await;
            }
            if status.success() {
                Ok(String::from_utf8_lossy(&stdout_buf).trim().to_string())
            } else {
                let mut stderr_buf = Vec::new();
                if let Some(ref mut err) = stderr {
                    use tokio::io::AsyncReadExt;
                    let _ = err.read_to_end(&mut stderr_buf).await;
                }
                Err(format!("adb 失败: {}", String::from_utf8_lossy(&stderr_buf).trim()))
            }
        },
        Ok(Err(e)) => Err(format!("等待 adb 失败: {}", e)),
        Err(_) => {
            // 超时：显式 kill + 等待回收
            let _ = child.kill().await;
            Err(format!("ADB 命令超时 ({}s)", timeout_secs))
        },
    }
}

async fn run_adb_async_raw(args: &[&str], timeout_secs: u64) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(adb_path());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }

    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("spawn adb 失败: {}", e))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait()).await {
        Ok(Ok(status)) => {
            let mut stdout_buf = Vec::new();
            if let Some(ref mut out) = stdout {
                use tokio::io::AsyncReadExt;
                let _ = out.read_to_end(&mut stdout_buf).await;
            }
            if status.success() {
                Ok(String::from_utf8_lossy(&stdout_buf).trim().to_string())
            } else {
                let mut stderr_buf = Vec::new();
                if let Some(ref mut err) = stderr {
                    use tokio::io::AsyncReadExt;
                    let _ = err.read_to_end(&mut stderr_buf).await;
                }
                let err = String::from_utf8_lossy(&stderr_buf).trim().to_string();
                let stdout = String::from_utf8_lossy(&stdout_buf).trim().to_string();
                if err.is_empty() {
                    Err(stdout)
                } else {
                    Err(err)
                }
            }
        },
        Ok(Err(e)) => Err(format!("等待 adb 失败: {}", e)),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(format!("ADB 命令超时 ({}s)", timeout_secs))
        },
    }
}

pub async fn adb_shell_async(serial: &str, command: &str) -> Result<String, String> {
    run_adb_async(serial, &["shell", command], crate::constants::timing::ADB_COMMAND_TIMEOUT_SECS)
        .await
}

/// 启用设备屏幕常亮：`adb shell svc power stayon true`
///
/// 在设备连上之后立即调用，使屏幕在连接期间不自动熄灭。
/// 说明：
/// - 该设置与 USB 会话绑定，设备断连后 Android 会自动清零，
///   因此每次设备重新上线都需再次调用（由 monitor 状态迁移检测触发）。
/// - 部分厂商 ROM 可能禁用 `svc power` 或需额外权限，失败时仅返回错误，
///   由调用方在日志层面静默处理，不影响设备注册与任务调度。
pub async fn enable_stayon_async(serial: &str) -> Result<(), String> {
    adb_shell_async(serial, "svc power stayon true").await.map(|_| ())
}

pub async fn adb_cmd_async(serial: &str, args: &[&str]) -> Result<String, String> {
    run_adb_async(serial, args, crate::constants::timing::ADB_COMMAND_TIMEOUT_SECS).await
}

pub async fn connect_wifi_via_adb_async(address: &str) -> Result<String, String> {
    let addr = parse_wifi_address(address)?;
    let addr_str = addr.to_string();

    let stdout = run_adb_async_raw(
        &["connect", &addr_str],
        crate::constants::timing::WIFI_CONNECT_TIMEOUT_SECS,
    )
    .await?;

    if stdout.contains("failed") {
        Err(format!("ADB WiFi 连接失败: {}", stdout.trim()))
    } else {
        Ok(format!("WiFi 设备已连接: {}", addr_str))
    }
}

// ─── ADB Port Forward ─────────────────────────────────────────────────────────

/// 建立 ADB TCP 端口转发：`adb -s {serial} forward tcp:{local_port} tcp:{PHONE_PORT_ON_DEVICE}`
///
/// 幂等：若转发已存在，ADB 会静默覆盖（同样参数）或返回端口号。
/// 失败不应阻断任务启动——调用方记录日志后继续即可。
pub async fn adb_forward_setup(serial: &str, local_port: u16) -> Result<(), String> {
    let remote_port = crate::constants::phone_client::PHONE_PORT_ON_DEVICE;
    let local_spec = format!("tcp:{}", local_port);
    let remote_spec = format!("tcp:{}", remote_port);
    run_adb_async(
        serial,
        &["forward", &local_spec, &remote_spec],
        crate::constants::timing::ADB_COMMAND_TIMEOUT_SECS,
    )
    .await
    .map(|out| {
        debug!(
            local_port = local_port,
            remote_port = remote_port,
            device = serial,
            "forward 已建立"
        );
        let _ = out; // ADB 成功时输出已分配的端口号，无需使用
    })
}

/// 移除 ADB TCP 端口转发：`adb -s {serial} forward --remove tcp:{local_port}`
///
/// 失败静默忽略（设备可能已断开，端口已自动释放）。
#[allow(dead_code)]
pub async fn adb_forward_remove(serial: &str, local_port: u16) {
    let local_spec = format!("tcp:{}", local_port);
    match run_adb_async(
        serial,
        &["forward", "--remove", &local_spec],
        crate::constants::timing::ADB_COMMAND_TIMEOUT_SECS,
    )
    .await
    {
        Ok(_) => {
            debug!(local_port = local_port, device = serial, "forward 已移除");
        },
        Err(e) => {
            debug!(
                device = serial,
                local_port = local_port,
                reason = %e,
                "forward 移除失败，设备可能已离线"
            );
        },
    }
}

/// 让设备 adbd 在 TCP `port` 上重启（`adb -s <serial> tcpip <port>`）。
///
/// 用于 USB → 无线切换前置：执行后约 1–2s adbd 才能在 wifi 接口监听，
/// 调用方需自行 sleep 等待。命令幂等：设备已在监听时再次执行也成功。
pub async fn enable_tcpip_async(serial: &str, port: u16) -> Result<(), String> {
    let port_str = port.to_string();
    run_adb_async(
        serial,
        &["tcpip", &port_str],
        crate::constants::timing::ADB_COMMAND_TIMEOUT_SECS,
    )
    .await
    .map(|_| ())
}

/// 通过 `adb shell ip -f inet addr show wlan0` 抓设备 wlan0 的 IPv4 地址。
///
/// 输出格式形如 `inet 192.168.110.142/24 brd ...`，按行扫 `inet ` 前缀，取
/// CIDR 之前的部分校验为合法 Ipv4 后返回。匹配不到则视为设备未连 WiFi。
pub async fn fetch_wlan_ipv4_async(serial: &str) -> Result<String, String> {
    let output = adb_shell_async(serial, "ip -f inet addr show wlan0").await?;
    for line in output.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("inet ") else { continue };
        let Some(cidr) = rest.split_whitespace().next() else { continue };
        let Some(ip) = cidr.split('/').next() else { continue };
        if ip.parse::<std::net::Ipv4Addr>().is_ok() {
            return Ok(ip.to_string());
        }
    }
    Err("设备未连接 WiFi 或无法获取 IP".to_string())
}

pub async fn disconnect_wifi_via_adb_async(serial: &str) -> Result<String, String> {
    if !serial.contains(':') {
        return Ok(format!("USB 设备无需断开 WiFi: {}", serial));
    }

    let stdout = run_adb_async_raw(
        &["disconnect", serial],
        crate::constants::timing::WIFI_CONNECT_TIMEOUT_SECS,
    )
    .await?;

    Ok(if stdout.is_empty() {
        format!("WiFi 设备已断开: {}", serial)
    } else {
        stdout
    })
}

pub async fn adb_keyboard_available(serial: &str) -> Result<bool, String> {
    let output = adb_shell_async(serial, "ime list -s").await?;
    Ok(output.lines().map(str::trim).any(|line| line == ADB_KEYBOARD_IME))
}

pub async fn adb_keyboard_input_text(serial: &str, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }

    let previous_ime = adb_shell_async(serial, "settings get secure default_input_method")
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    let should_restore = !previous_ime.is_empty() && previous_ime != ADB_KEYBOARD_IME;

    if previous_ime != ADB_KEYBOARD_IME {
        adb_shell_async(serial, &format!("ime set {}", ADB_KEYBOARD_IME)).await?;
        tokio::time::sleep(std::time::Duration::from_millis(ADB_KEYBOARD_SWITCH_DELAY_MS)).await;
    }

    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let broadcast_cmd = format!("am broadcast -a ADB_INPUT_B64 --es msg '{}'", encoded);
    let broadcast_result = adb_shell_async(serial, &broadcast_cmd).await;

    if should_restore {
        tokio::time::sleep(std::time::Duration::from_millis(ADB_KEYBOARD_RESTORE_DELAY_MS)).await;
        let _ = adb_shell_async(serial, &format!("ime set {}", previous_ime)).await;
    }

    let output = broadcast_result?;
    if output.contains("Broadcast completed") || output.contains("result=0") {
        Ok(())
    } else {
        Err(format!("ADBKeyBoard 输入失败: {}", output.trim()))
    }
}

/// 通过 adb CLI 执行 shell 命令（带超时保护）
pub(crate) fn adb_shell(serial: &str, command: &str) -> Result<String, String> {
    let output = run_adb_timed(
        adb_command().args(["-s", serial, "shell", command]),
        crate::constants::timing::ADB_COMMAND_TIMEOUT_SECS,
    )?;

    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|e| format!("输出解码失败: {}", e))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("命令执行失败: {}", stderr.trim()))
    }
}

/// ADB 批量命令合并：将多个 shell 命令合并为单次 TCP 连接执行
///
/// 用 `echo '__SEP__'` 分隔各命令的输出，返回按序的结果 Vec。
/// 优势：N 个命令只建一次 TCP 连接（原先每个命令独立 TCP 握手）。
#[allow(dead_code)]
pub fn batch_shell_commands(serial: &str, commands: &[&str]) -> Vec<Result<String, String>> {
    if commands.is_empty() {
        return Vec::new();
    }
    if commands.len() == 1 {
        return vec![adb_shell(serial, commands[0])];
    }

    const SEP: &str = "__SEP__";
    let combined = commands
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(&format!(" && echo '{}' && ", SEP));

    match adb_shell(serial, &combined) {
        Ok(output) => {
            let parts: Vec<&str> = output.split(SEP).collect();
            parts.iter().map(|p| Ok(p.trim().to_string())).collect()
        },
        Err(e) => {
            // 合并执行失败时，对所有命令返回相同错误
            commands.iter().map(|_| Err(e.clone())).collect()
        },
    }
}

/// 通过 adb CLI 执行非 shell 命令（带超时保护）
#[allow(dead_code)]
pub(crate) fn adb_cmd(serial: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = adb_command();
    cmd.args(["-s", serial]);
    cmd.args(args);

    let output = run_adb_timed(&mut cmd, crate::constants::timing::ADB_COMMAND_TIMEOUT_SECS)?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("执行失败: {}", stderr.trim()))
    }
}

/// 设备震动警告（10次短震）
///
/// 异步 fire-and-forget：spawn 独立 task 执行，不阻塞引擎事件循环。
/// 震动失败静默忽略（设备可能已离线或不支持震动）。
///
/// 模式：200ms 震 → 150ms 停 × 10 次 ≈ 3.5 秒
///
/// 兼容策略：优先 `cmd vibrator vibrate`（Android 8+），
/// 失败后回退 `service call vibrator`（Android 7 及以下）。
pub fn vibrate_device_alert(serial: &str) {
    let serial = serial.to_string();
    tokio::spawn(async move {
        const VIBRATE_MS: u64 = 200;
        const PAUSE_MS: u64 = 150;
        const REPEAT: usize = 10;

        // 探测震动方式：首次尝试确定可用命令，后续复用
        let vibrate_cmd = match probe_vibrate_cmd(&serial, VIBRATE_MS).await {
            Some(cmd) => cmd,
            None => {
                debug!(device = %serial, "设备不支持震动或已离线，跳过警告");
                return;
            },
        };

        for i in 1..REPEAT {
            if i < REPEAT - 1 {
                tokio::time::sleep(std::time::Duration::from_millis(PAUSE_MS + VIBRATE_MS)).await;
            }
            if adb_shell_async(&serial, &vibrate_cmd).await.is_err() {
                return; // 设备不可达，提前退出
            }
        }

        debug!(device = %serial, count = REPEAT, "震动警告完成");
    });
}

/// 探测设备可用的震动命令，返回 None 表示不支持
async fn probe_vibrate_cmd(serial: &str, ms: u64) -> Option<String> {
    // Android 8+: cmd vibrator vibrate
    let cmd_vibrator = format!("cmd vibrator vibrate {}", ms);
    if adb_shell_async(serial, &cmd_vibrator).await.is_ok() {
        return Some(cmd_vibrator);
    }
    // Android 7 及以下: service call vibrator 2 i32 <ms>
    let service_call = format!("service call vibrator 2 i32 {}", ms);
    if adb_shell_async(serial, &service_call).await.is_ok() {
        return Some(service_call);
    }
    None
}
