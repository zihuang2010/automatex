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
