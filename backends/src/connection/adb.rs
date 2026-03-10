//! ADB 底层辅助函数
//!
//! 负责 adb 路径发现、命令构建、超时执行等底层操作。
//! DeviceManager 方法通过本模块与 ADB 交互。

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

/// 二进制完整性校验：验证 sidecar 文件存在且大小合理
///
/// 检测点：文件存在、大小 > 100KB（防截断）、可读。
/// 在启动时调用一次即可，结果缓存在日志中。
pub fn verify_sidecar_integrity() {
    let exe_dir = match std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        Some(d) => d,
        None => {
            eprintln!("[integrity] 无法获取可执行文件目录");
            return;
        }
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
            }
            Err(_) => {
                eprintln!("[integrity] ⚠ {} 未找到: {:?}", name, path);
            }
        }
    }
}

/// 创建不弹出控制台窗口的 ADB Command（Windows 上设置 CREATE_NO_WINDOW）
#[allow(unused_mut)]
pub fn adb_command() -> std::process::Command {
    let mut cmd = std::process::Command::new(adb_path());
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

    let child = cmd.spawn().map_err(|e| format!("spawn adb 失败: {}", e))?;

    // 内核事件驱动等待（epoll/kqueue），非轮询
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| format!("ADB 命令超时 ({}s)", timeout_secs))?
    .map_err(|e| format!("等待 adb 失败: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "adb 失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
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
            parts
                .iter()
                .map(|p| Ok(p.trim().to_string()))
                .collect()
        }
        Err(e) => {
            // 合并执行失败时，对所有命令返回相同错误
            commands.iter().map(|_| Err(e.clone())).collect()
        }
    }
}

/// 通过 adb CLI 执行非 shell 命令（带超时保护）
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
