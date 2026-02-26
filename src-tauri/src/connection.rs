use adb_client::server::ADBServer;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Mutex;

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

/// 返回给前端的设备信息
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeviceInfo {
    pub serial: String,
    pub name: String,
    pub state: String,
    pub device_type: String,
}

/// Shell 命令执行结果
#[derive(Debug, Serialize, Deserialize)]
pub struct ShellResult {
    pub success: bool,
    pub output: String,
    pub error: String,
}

/// 设备详细属性
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceProperties {
    pub serial: String,
    pub model: String,
    pub brand: String,
    pub android_version: String,
    pub sdk_version: String,
    pub display_resolution: String,
    pub device_type: String,
    pub battery_level: i32,
    pub battery_temperature: f64,
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

    /// 从持久化数据恢复设备列表
    pub fn restore_devices(&self, entries: Vec<DeviceEntry>) {
        let mut devices = self.devices.lock().unwrap();
        *devices = entries;
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

    /// 获取所有设备列表
    pub fn get_devices(&self) -> Vec<DeviceEntry> {
        self.devices.lock().unwrap().clone()
    }

    /// 清空所有设备（断开所有 ADB 连接，包括自动发现的）
    pub fn clear_devices(&self) {
        // 先从 ADB Server 获取当前所有连接的设备，逐个断开
        if let Ok(mut server) = std::panic::catch_unwind(|| ADBServer::new(adb_server_addr())) {
            if let Ok(devs) = server.devices() {
                for dev in &devs {
                    let serial = dev.identifier.to_string();
                    // 断开所有非 USB 设备（WiFi / mDNS / TLS transport）
                    let _ = std::process::Command::new("adb")
                        .args(["disconnect", &serial])
                        .output();
                }
            }
        }
        // 兜底：执行 adb disconnect（无参数断开所有远程连接）
        let _ = std::process::Command::new("adb").arg("disconnect").output();

        // 清空手动列表
        self.devices.lock().unwrap().clear();
    }

    /// 移除设备（同时断开 WiFi 连接）
    pub fn remove_device_and_disconnect(&self, serial: &str) -> Result<(), String> {
        // 先断开 WiFi 连接
        if serial.contains(':') {
            let _ = std::process::Command::new("adb")
                .args(["disconnect", serial])
                .output();
        }
        // 从手动列表中移除
        let mut devices = self.devices.lock().unwrap();
        devices.retain(|d| d.serial != serial && d.address.as_deref() != Some(serial));
        Ok(())
    }

    /// 通过 ADB Server 扫描所有连接的设备（USB + WiFi），自动去重
    pub fn scan_adb_devices(&self) -> Result<Vec<DeviceInfo>, String> {
        let mut server = ADBServer::new(adb_server_addr());

        let adb_devices = server
            .devices()
            .map_err(|e| format!("ADB Server 连接失败 (确保 adb 已安装并运行): {}", e))?;

        let manual_devices = self.devices.lock().unwrap();

        // 第一步：收集所有 ADB 设备信息，并查询硬件序列号用于去重
        struct RawDevice {
            serial: String,
            state: String,
            device_type: String,
            hw_serial: Option<String>, // ro.serialno 用于去重
        }

        let mut raw_devices: Vec<RawDevice> = Vec::new();
        for dev in &adb_devices {
            let serial = dev.identifier.to_string();
            let state = format!("{:?}", dev.state);
            let device_type = if serial.contains(':') {
                "wifi".to_string()
            } else {
                "usb".to_string()
            };

            // 尝试获取硬件序列号用于去重
            let hw_serial = adb_shell(&serial, "getprop ro.serialno")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            raw_devices.push(RawDevice {
                serial,
                state,
                device_type,
                hw_serial,
            });
        }

        // 第二步：按硬件序列号去重，同一物理设备只保留一个条目
        let mut result: Vec<DeviceInfo> = Vec::new();
        let mut seen_hw_serials: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // 优先处理 WiFi 设备（如果在手动列表中有记录）
        let mut wifi_first: Vec<&RawDevice> = raw_devices.iter().collect();
        wifi_first.sort_by_key(|d| if d.device_type == "wifi" { 0 } else { 1 });

        for dev in wifi_first {
            // 如果有硬件序列号且已处理过，跳过（同一物理设备）
            if let Some(ref hw) = dev.hw_serial {
                if !seen_hw_serials.insert(hw.clone()) {
                    continue; // 重复设备，跳过
                }
            }

            // 在手动列表中查找名称
            let name = manual_devices
                .iter()
                .find(|d| d.serial == dev.serial || d.address.as_deref() == Some(&dev.serial))
                .map(|d| d.name.clone())
                .unwrap_or_else(|| dev.serial.clone());

            result.push(DeviceInfo {
                serial: dev.serial.clone(),
                name,
                state: dev.state.clone(),
                device_type: dev.device_type.clone(),
            });
        }

        // 第三步：添加手动录入但 ADB Server 未发现的设备（标记为 Offline）
        for entry in manual_devices.iter() {
            let already = result.iter().any(|d| {
                d.serial == entry.serial
                    || entry
                        .address
                        .as_deref()
                        .map(|a| d.serial == a)
                        .unwrap_or(false)
            });
            if !already {
                result.push(DeviceInfo {
                    serial: entry.serial.clone(),
                    name: entry.name.clone(),
                    state: "Offline".to_string(),
                    device_type: match entry.device_type {
                        DeviceType::Usb => "usb".to_string(),
                        DeviceType::Wifi => "wifi".to_string(),
                    },
                });
            }
        }

        Ok(result)
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

    /// 获取设备详细信息（通过 adb CLI 直接调用）
    pub fn get_device_info(&self, serial: &str) -> Result<DeviceProperties, String> {
        let device_type = if serial.contains(':') { "wifi" } else { "usb" };

        let model =
            adb_shell(serial, "getprop ro.product.model").unwrap_or_else(|_| "unknown".into());
        let brand =
            adb_shell(serial, "getprop ro.product.brand").unwrap_or_else(|_| "unknown".into());
        let android_version = adb_shell(serial, "getprop ro.build.version.release")
            .unwrap_or_else(|_| "unknown".into());
        let sdk_version =
            adb_shell(serial, "getprop ro.build.version.sdk").unwrap_or_else(|_| "unknown".into());
        let display_resolution = adb_shell(serial, "wm size").unwrap_or_else(|_| "unknown".into());

        // 获取电池信息
        let battery_dump = adb_shell(serial, "dumpsys battery").unwrap_or_default();
        let battery_level = parse_battery_field(&battery_dump, "level").unwrap_or(0);
        let battery_temp_raw = parse_battery_field(&battery_dump, "temperature").unwrap_or(250);
        let battery_temperature = battery_temp_raw as f64 / 10.0;

        Ok(DeviceProperties {
            serial: serial.to_string(),
            model: model.trim().to_string(),
            brand: brand.trim().to_string(),
            android_version: android_version.trim().to_string(),
            sdk_version: sdk_version.trim().to_string(),
            display_resolution: display_resolution.trim().to_string(),
            device_type: device_type.to_string(),
            battery_level,
            battery_temperature,
        })
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

        let output = std::process::Command::new("adb")
            .args(["connect", &addr_str])
            .output()
            .map_err(|e| format!("执行 adb connect 失败: {}", e))?;

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
    let output = std::process::Command::new("adb")
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
    let mut cmd = std::process::Command::new("adb");
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

/// 从 `dumpsys battery` 输出中解析指定字段的整数值
fn parse_battery_field(dump: &str, field: &str) -> Option<i32> {
    for line in dump.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(field) {
            let rest = rest.trim_start();
            if let Some(val) = rest.strip_prefix(':') {
                return val.trim().parse::<i32>().ok();
            }
        }
    }
    None
}
