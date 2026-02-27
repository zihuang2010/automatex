use adb_client::server::ADBServer;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Mutex, OnceLock};

/// 获取内嵌 adb 的路径（Tauri sidecar，与可执行文件同目录）
pub fn adb_path() -> &'static str {
    static ADB: OnceLock<String> = OnceLock::new();
    ADB.get_or_init(|| {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                // Windows 上查找 adb.exe，macOS/Linux 上查找 adb
                let sidecar = if cfg!(windows) {
                    dir.join("adb.exe")
                } else {
                    dir.join("adb")
                };
                if sidecar.exists() {
                    return sidecar.to_string_lossy().to_string();
                }
            }
        }
        "adb".to_string() // 兜底：开发环境走系统 PATH
    })
}

/// 创建不弹出控制台窗口的 ADB Command（Windows 上设置 CREATE_NO_WINDOW）
#[allow(unused_mut)]
pub fn adb_command() -> std::process::Command {
    let mut cmd = std::process::Command::new(adb_path());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

// ─── Data Types ─────────────────────────────────────────────────

/// 设备连接类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Usb,
    Wifi,
}

/// 持久化的设备条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceEntry {
    pub serial: String,
    pub name: String,
    pub device_type: DeviceType,
    /// WiFi 设备的连接地址（USB 设备为 None）
    pub address: Option<String>,
}

/// Shell 命令执行结果
#[derive(Debug, Serialize, Deserialize)]
pub struct ShellResult {
    pub success: bool,
    pub output: String,
    pub error: String,
}

// ─── ADB Server 连接地址 ─────────────────────────────────────────

const ADB_SERVER_PORT: u16 = 5037;

fn adb_server_addr() -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), ADB_SERVER_PORT)
}

// ─── Device Manager ─────────────────────────────────────────────

pub struct DeviceManager {
    /// 用户手动添加的设备列表（WiFi 设备）
    pub devices: Mutex<Vec<DeviceEntry>>,
}

impl DeviceManager {
    pub fn new() -> Self {
        Self {
            devices: Mutex::new(Vec::new()),
        }
    }

    /// 添加 WiFi 设备
    pub fn add_wifi_device(&self, address: &str, name: &str) -> Result<DeviceEntry, String> {
        let addr = parse_wifi_address(address)?;
        let normalized = addr.to_string();

        let mut devices = self.devices.lock().unwrap();

        // 检查是否已添加
        if devices
            .iter()
            .any(|d| d.address.as_deref() == Some(&normalized) || d.serial == normalized)
        {
            return Err(format!("设备 {} 已存在", normalized));
        }

        let entry_name = if name.is_empty() {
            normalized.clone()
        } else {
            name.to_string()
        };

        let entry = DeviceEntry {
            serial: normalized.clone(),
            name: entry_name,
            device_type: DeviceType::Wifi,
            address: Some(normalized),
        };

        devices.push(entry.clone());
        Ok(entry)
    }

    /// 清空所有设备（断开所有 ADB 连接，包括自动发现的）
    pub fn clear_devices(&self) {
        // Phase 1: 不持锁执行 ADB 断开操作
        if let Ok(mut server) = std::panic::catch_unwind(|| ADBServer::new(adb_server_addr())) {
            if let Ok(devs) = server.devices() {
                for dev in &devs {
                    let serial = dev.identifier.to_string();
                    let _ = adb_command().args(["disconnect", &serial]).output();
                }
            }
        }
        let _ = adb_command().arg("disconnect").output();

        // Phase 2: 持锁清空手动列表（瞬时操作）
        self.devices.lock().unwrap().clear();
    }

    /// 移除设备（同时断开 WiFi 连接）
    pub fn remove_device_and_disconnect(&self, serial: &str) -> Result<(), String> {
        // Phase 1: 不持锁执行 ADB 断开（可能阻塞）
        if serial.contains(':') {
            let _ = adb_command().args(["disconnect", serial]).output();
        }
        // Phase 2: 持锁移除（瞬时操作）
        let mut devices = self.devices.lock().unwrap();
        devices.retain(|d| d.serial != serial && d.address.as_deref() != Some(serial));
        Ok(())
    }

    /// 在设备上执行 shell 命令
    pub fn execute_shell(&self, serial: &str, command: &str) -> ShellResult {
        match adb_shell(serial, command) {
            Ok(output) => ShellResult {
                success: true,
                output: output.trim().to_string(),
                error: String::new(),
            },
            Err(e) => ShellResult {
                success: false,
                output: String::new(),
                error: e,
            },
        }
    }

    /// 安装 APK
    pub fn install_apk(&self, serial: &str, apk_path: &str) -> Result<String, String> {
        adb_cmd(serial, &["install", apk_path]).map(|_| format!("APK 安装成功: {}", apk_path))
    }

    /// 重启设备
    pub fn reboot_device(&self, serial: &str) -> Result<String, String> {
        adb_cmd(serial, &["reboot"]).map(|_| "设备正在重启...".to_string())
    }

    /// 推送文件到设备
    pub fn push_file(
        &self,
        serial: &str,
        local_path: &str,
        remote_path: &str,
    ) -> Result<String, String> {
        adb_cmd(serial, &["push", local_path, remote_path])
            .map(|_| format!("文件已推送: {} -> {}", local_path, remote_path))
    }

    /// 从设备拉取文件
    pub fn pull_file(
        &self,
        serial: &str,
        remote_path: &str,
        local_path: &str,
    ) -> Result<String, String> {
        adb_cmd(serial, &["pull", remote_path, local_path])
            .map(|_| format!("文件已拉取: {} -> {}", remote_path, local_path))
    }

    /// 通过 adb connect 连接 WiFi 设备
    pub fn connect_wifi_via_adb(&self, address: &str) -> Result<String, String> {
        let addr = parse_wifi_address(address)?;
        let addr_str = addr.to_string();

        let mut child = adb_command()
            .args(["connect", &addr_str])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("执行 adb connect 失败: {}", e))?;

        // 5 秒超时
        let timeout = std::time::Duration::from_secs(5);
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
                }
                Err(e) => return Err(format!("等待 adb connect 失败: {}", e)),
            }
        }

        let output = child
            .wait_with_output()
            .map_err(|e| format!("读取 adb connect 输出失败: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && !stdout.contains("failed") {
            Ok(format!("WiFi 设备已连接: {}", addr_str))
        } else {
            Err(format!("ADB WiFi 连接失败: {}", stdout.trim()))
        }
    }
}

// ─── Helpers ───────────────────────────────────────────────────

/// 解析 WiFi 地址，不含端口时默认 5555
fn parse_wifi_address(addr: &str) -> Result<std::net::SocketAddr, String> {
    let full = if addr.contains(':') {
        addr.to_string()
    } else {
        format!("{}:5555", addr)
    };
    full.parse::<std::net::SocketAddr>()
        .map_err(|e| format!("地址格式错误 '{}': {}", addr, e))
}

/// 通过 adb CLI 执行 shell 命令（最可靠方式）
fn adb_shell(serial: &str, command: &str) -> Result<String, String> {
    let output = adb_command()
        .args(["-s", serial, "shell", command])
        .output()
        .map_err(|e| format!("执行 adb 失败 (确保 adb 已安装): {}", e))?;

    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|e| format!("输出解码失败: {}", e))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("命令执行失败: {}", stderr.trim()))
    }
}

/// 通过 adb CLI 执行非 shell 命令
fn adb_cmd(serial: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = adb_command();
    cmd.args(["-s", serial]);
    cmd.args(args);

    let output = cmd.output().map_err(|e| format!("执行 adb 失败: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("执行失败: {}", stderr.trim()))
    }
}
