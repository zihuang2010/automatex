//! 设备连接管理模块
//!
//! - `types.rs` — DeviceType、DeviceEntry、ShellResult 数据结构
//! - `adb.rs` — ADB 底层辅助函数
//! - `mod.rs` — DeviceManager 设备管理器

pub mod adb;
pub mod types;

pub use adb::{adb_command, run_adb_timed};
pub use types::{DeviceEntry, DeviceType, ShellResult};

use adb::{adb_cmd, adb_shell, parse_wifi_address};

// ─── Device Manager ─────────────────────────────────────────────

pub struct DeviceManager;

impl DeviceManager {
    #[allow(dead_code)]
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
    pub fn install_apk(&self, serial: &str, apk_path: &str) -> Result<String, String> {
        adb_cmd(serial, &["install", apk_path]).map(|_| format!("APK 安装成功: {}", apk_path))
    }

    #[allow(dead_code)]
    pub fn reboot_device(&self, serial: &str) -> Result<String, String> {
        adb_cmd(serial, &["reboot"]).map(|_| "设备正在重启...".to_string())
    }

    #[allow(dead_code)]
    pub fn push_file(
        &self,
        serial: &str,
        local_path: &str,
        remote_path: &str,
    ) -> Result<String, String> {
        adb_cmd(serial, &["push", local_path, remote_path])
            .map(|_| format!("文件已推送: {} -> {}", local_path, remote_path))
    }

    #[allow(dead_code)]
    pub fn pull_file(
        &self,
        serial: &str,
        remote_path: &str,
        local_path: &str,
    ) -> Result<String, String> {
        adb_cmd(serial, &["pull", remote_path, local_path])
            .map(|_| format!("文件已拉取: {} -> {}", remote_path, local_path))
    }

    #[allow(dead_code)]
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
