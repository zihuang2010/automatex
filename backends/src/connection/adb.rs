//! ADB 底层辅助函数
//!
//! 负责 adb 路径发现、命令构建、超时执行等底层操作。
//! DeviceManager 方法通过本模块与 ADB 交互。

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::OnceLock;

/// 获取内嵌 adb 的路径（Tauri sidecar，与可执行文件同目录）
pub fn adb_path() -> &'static str {
    static ADB: OnceLock<String> = OnceLock::new();
    ADB.get_or_init(|| {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let sidecar = if cfg!(windows) { dir.join("adb.exe") } else { dir.join("adb") };
                if sidecar.exists() {
                    return sidecar.to_string_lossy().to_string();
                }
            }
        }
        "adb".to_string()
    })
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
                                eprintln!(
                                    "[adb] 使用已有 ADB server: 端口 {} (默认 {} 不可用)",
                                    port, default_port
                                );
                            } else {
                                eprintln!("[adb] ADB server 已在端口 {} 运行", port);
                            }
                            return;
                        }
                    }
                }
                // 端口被非 ADB 进程占用，跳过
                eprintln!("[adb] 端口 {} 被非 ADB 进程占用，尝试下一个...", port);
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
                            eprintln!(
                                "[adb] 在端口 {} 启动 ADB server (默认 {} 不可用)",
                                port, default_port
                            );
                        } else {
                            eprintln!("[adb] ADB server 已在端口 {} 启动", port);
                        }
                        return;
                    },
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        eprintln!("[adb] 端口 {} 启动失败: {}", port, stderr.trim());
                    },
                    Err(e) => {
                        eprintln!("[adb] 端口 {} 启动失败: {}", port, e);
                    },
                }
            },
        }
    }

    // 所有端口均不可用，保持默认值并打印警告
    eprintln!(
        "[adb] ⚠ 端口 {}-{} 均不可用，使用默认端口 {}（可能无法正常工作）",
        default_port, max_port, default_port
    );
}

/// 二进制完整性校验：验证 sidecar 文件存在且大小合理
///
/// 检测点：文件存在、大小 > 100KB（防截断）、可读。
/// Windows 额外检查 AdbWinApi.dll 和 AdbWinUsbApi.dll。
/// 在启动时调用一次即可，结果缓存在日志中。
pub fn verify_sidecar_integrity() {
    let exe_dir =
        match std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
            Some(d) => d,
            None => {
                eprintln!("[integrity] 无法获取可执行文件目录");
                return;
            },
        };

    let adb_name = if cfg!(windows) { "adb.exe" } else { "adb" };
    let sidecars = [adb_name, "scrcpy-server"];

    for name in &sidecars {
        let path = exe_dir.join(name);
        match std::fs::metadata(&path) {
            Ok(meta) => {
                let size = meta.len();
                if size < 100_000 {
                    eprintln!(
                        "[integrity] ⚠ {} 文件异常: 大小仅 {} 字节 (可能被截断/替换)",
                        name, size
                    );
                } else {
                    eprintln!("[integrity] ✓ {} ({} bytes)", name, size);
                }
            },
            Err(_) => {
                eprintln!("[integrity] ⚠ {} 未找到: {:?}", name, path);
            },
        }
    }

    // Windows: 检查 ADB 运行时 DLL 依赖
    #[cfg(windows)]
    {
        let dlls = ["AdbWinApi.dll", "AdbWinUsbApi.dll"];
        for dll in &dlls {
            let path = exe_dir.join(dll);
            if path.exists() {
                eprintln!("[integrity] ✓ {}", dll);
            } else {
                eprintln!("[integrity] ⚠ {} 未找到: {:?} — adb.exe 可能无法正常运行！", dll, path);
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
                std::thread::sleep(std::time::Duration::from_millis(100));
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
        use std::os::windows::process::CommandExt;
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
        use std::os::windows::process::CommandExt;
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
