use serde::{Deserialize, Serialize};
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

// ─── Data Types ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Usb,
    Wifi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceEntry {
    pub serial: String,
    pub name: String,
    pub device_type: DeviceType,
    pub address: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ShellResult {
    pub success: bool,
    pub output: String,
    pub error: String,
}

// ─── Device Manager ─────────────────────────────────────────────

pub struct DeviceManager;

impl DeviceManager {
    pub fn new() -> Self {
        Self
    }

    pub fn build_wifi_entry(address: &str, name: &str) -> Result<DeviceEntry, String> {
        let addr = parse_wifi_address(address)?;
        let normalized = addr.to_string();
        let entry_name = if name.is_empty() { normalized.clone() } else { name.to_string() };
        Ok(DeviceEntry {
            serial: normalized.clone(),
            name: entry_name,
            device_type: DeviceType::Wifi,
            address: Some(normalized),
        })
    }

    pub fn disconnect_wifi(serial: &str) {
        if serial.contains(':') {
            let _ = adb_command().args(["disconnect", serial]).output();
        }
    }

    pub fn execute_shell(&self, serial: &str, command: &str) -> ShellResult {
        match adb_shell(serial, command) {
            Ok(output) => ShellResult {
                success: true,
                output: output.trim().to_string(),
                error: String::new(),
            },
            Err(e) => ShellResult { success: false, output: String::new(), error: e },
        }
    }

    pub fn install_apk(&self, serial: &str, apk_path: &str) -> Result<String, String> {
        adb_cmd(serial, &["install", apk_path]).map(|_| format!("APK 安装成功: {}", apk_path))
    }

    pub fn reboot_device(&self, serial: &str) -> Result<String, String> {
        adb_cmd(serial, &["reboot"]).map(|_| "设备正在重启...".to_string())
    }

    pub fn push_file(
        &self,
        serial: &str,
        local_path: &str,
        remote_path: &str,
    ) -> Result<String, String> {
        adb_cmd(serial, &["push", local_path, remote_path])
            .map(|_| format!("文件已推送: {} -> {}", local_path, remote_path))
    }

    pub fn pull_file(
        &self,
        serial: &str,
        remote_path: &str,
        local_path: &str,
    ) -> Result<String, String> {
        adb_cmd(serial, &["pull", remote_path, local_path])
            .map(|_| format!("文件已拉取: {} -> {}", remote_path, local_path))
    }

    pub fn connect_wifi_via_adb(&self, address: &str) -> Result<String, String> {
        let addr = parse_wifi_address(address)?;
        let addr_str = addr.to_string();

        let mut child = adb_command()
            .args(["connect", &addr_str])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("执行 adb connect 失败: {}", e))?;

        let timeout =
            std::time::Duration::from_secs(crate::constants::timing::WIFI_CONNECT_TIMEOUT_SECS);
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if start.elapsed() > timeout {
                        let _ = child.kill();
                        return Err(format!(
                            "WiFi 连接超时 ({}s): {}",
                            timeout.as_secs(),
                            addr_str
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                },
                Err(e) => return Err(format!("等待 adb connect 失败: {}", e)),
            }
        }

        let output =
            child.wait_with_output().map_err(|e| format!("读取 adb connect 输出失败: {}", e))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && !stdout.contains("failed") {
            Ok(format!("WiFi 设备已连接: {}", addr_str))
        } else {
            Err(format!("ADB WiFi 连接失败: {}", stdout.trim()))
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────

fn parse_wifi_address(addr: &str) -> Result<std::net::SocketAddr, String> {
    let full = if addr.contains(':') { addr.to_string() } else { format!("{}:5555", addr) };
    full.parse::<std::net::SocketAddr>().map_err(|e| format!("地址格式错误 '{}': {}", addr, e))
}

/// FIX #4: 带超时的 ADB 命令执行（防止进程永久阻塞）
fn run_adb_timed(
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
fn adb_shell(serial: &str, command: &str) -> Result<String, String> {
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
fn adb_cmd(serial: &str, args: &[&str]) -> Result<String, String> {
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
