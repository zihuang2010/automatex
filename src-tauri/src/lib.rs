mod connection;
mod mqtt;
mod storage;

use connection::{DeviceInfo, DeviceManager, DeviceProperties, ShellResult};
use mqtt::{MqttConfig, MqttManager, MqttStatus};
use std::sync::Arc;
use tauri::{Emitter, Manager};

// ─── State ─────────────────────────────────────────────────────

struct AppState {
    manager: DeviceManager,
    db: storage::Database,
    mqtt: Arc<MqttManager>,
}

// ─── Tauri Commands ────────────────────────────────────────────

/// 添加 WiFi 设备（IP:Port）
#[tauri::command]
fn add_device(
    address: String,
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let entry = state.manager.add_wifi_device(&address, &name)?;

    // 尝试通过 ADB 连接该 WiFi 设备
    let _ = state.manager.connect_wifi_via_adb(&address);

    // 持久化到 SQLite
    state.db.save_device(&entry);

    Ok(format!("设备 {} 已添加", entry.serial))
}

/// 移除设备（同时断开 WiFi 连接）
#[tauri::command]
fn remove_device(serial: String, state: tauri::State<'_, AppState>) -> Result<String, String> {
    state.manager.remove_device_and_disconnect(&serial)?;
    state.db.delete_device(&serial);
    Ok(format!("设备 {} 已移除", serial))
}

/// 列出所有设备（ADB 扫描 + DB fallback）
#[tauri::command]
fn list_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceInfo>, String> {
    match state.manager.scan_adb_devices() {
        Ok(devices) => {
            // 扫描成功：将在线设备同步到数据库
            for dev in &devices {
                if dev.state != "Offline" {
                    let device_type = if dev.device_type == "wifi" {
                        connection::DeviceType::Wifi
                    } else {
                        connection::DeviceType::Usb
                    };
                    let address = if dev.device_type == "wifi" {
                        Some(dev.serial.clone())
                    } else {
                        None
                    };
                    let entry = connection::DeviceEntry {
                        serial: dev.serial.clone(),
                        name: dev.name.clone(),
                        device_type,
                        address,
                    };
                    state.db.save_device(&entry);
                }
            }
            Ok(devices)
        }
        Err(_) => {
            // ADB Server 不可用：从数据库加载已保存的设备（全标记 Offline）
            let saved = state.db.load_devices();
            let devices: Vec<DeviceInfo> = saved
                .into_iter()
                .map(|entry| DeviceInfo {
                    serial: entry.serial,
                    name: entry.name,
                    state: "Offline".to_string(),
                    device_type: match entry.device_type {
                        connection::DeviceType::Usb => "usb".to_string(),
                        connection::DeviceType::Wifi => "wifi".to_string(),
                    },
                })
                .collect();
            Ok(devices)
        }
    }
}

/// 在指定设备上执行 Shell 命令
#[tauri::command]
fn execute_shell(
    serial: String,
    command: String,
    state: tauri::State<'_, AppState>,
) -> ShellResult {
    state.manager.execute_shell(&serial, &command)
}

/// 获取设备详细信息
#[tauri::command]
fn get_device_info(
    serial: String,
    state: tauri::State<'_, AppState>,
) -> Result<DeviceProperties, String> {
    state.manager.get_device_info(&serial)
}

/// 安装 APK
#[tauri::command]
fn install_apk(
    serial: String,
    apk_path: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.manager.install_apk(&serial, &apk_path)
}

/// 重启设备
#[tauri::command]
fn reboot_device(serial: String, state: tauri::State<'_, AppState>) -> Result<String, String> {
    state.manager.reboot_device(&serial)
}

/// 推送文件到设备
#[tauri::command]
fn push_file(
    serial: String,
    local_path: String,
    remote_path: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.manager.push_file(&serial, &local_path, &remote_path)
}

/// 从设备拉取文件
#[tauri::command]
fn pull_file(
    serial: String,
    remote_path: String,
    local_path: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.manager.pull_file(&serial, &remote_path, &local_path)
}

/// 移除所有设备
#[tauri::command]
fn remove_all_devices(state: tauri::State<'_, AppState>) -> Result<String, String> {
    state.manager.clear_devices();
    state.db.clear_devices();
    Ok("已移除所有设备".to_string())
}

// ─── Settings Commands ─────────────────────────────────────────

/// 获取设置
#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> serde_json::Value {
    let mqtt_host = state.db.get_setting("mqtt_host").unwrap_or_default();
    let mqtt_port = state
        .db
        .get_setting("mqtt_port")
        .unwrap_or_else(|| "1883".to_string());
    let mqtt_client_id = state
        .db
        .get_setting("mqtt_client_id")
        .unwrap_or_else(|| format!("automatex-{}", std::process::id()));
    let mqtt_username = state.db.get_setting("mqtt_username").unwrap_or_default();
    let mqtt_password = state.db.get_setting("mqtt_password").unwrap_or_default();

    serde_json::json!({
        "mqtt_host": mqtt_host,
        "mqtt_port": mqtt_port,
        "mqtt_client_id": mqtt_client_id,
        "mqtt_username": mqtt_username,
        "mqtt_password": mqtt_password,
    })
}

/// 保存设置
#[tauri::command]
fn save_settings(
    settings: serde_json::Value,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if let Some(obj) = settings.as_object() {
        for (key, value) in obj {
            let v = value.as_str().unwrap_or("");
            state.db.set_setting(key, v);
        }
    }
    Ok("设置已保存".to_string())
}

// ─── MQTT Commands ─────────────────────────────────────────────

/// 连接 MQTT
#[tauri::command]
async fn mqtt_connect(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let host = state
        .db
        .get_setting("mqtt_host")
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = state
        .db
        .get_setting("mqtt_port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1883);
    let client_id = state
        .db
        .get_setting("mqtt_client_id")
        .unwrap_or_else(|| format!("automatex-{}", std::process::id()));
    let username = state
        .db
        .get_setting("mqtt_username")
        .filter(|s| !s.is_empty());
    let password = state
        .db
        .get_setting("mqtt_password")
        .filter(|s| !s.is_empty());

    let config = MqttConfig {
        broker_host: host,
        broker_port: port,
        client_id,
        username,
        password,
    };

    state.mqtt.connect(config, app).await
}

/// 断开 MQTT
#[tauri::command]
async fn mqtt_disconnect(state: tauri::State<'_, AppState>) -> Result<String, String> {
    state.mqtt.disconnect().await
}

/// 订阅 MQTT 主题
#[tauri::command]
async fn mqtt_subscribe(
    topic: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.mqtt.subscribe(&topic).await
}

/// 发布 MQTT 消息
#[tauri::command]
async fn mqtt_publish(
    topic: String,
    payload: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.mqtt.publish(&topic, &payload).await
}

/// 获取 MQTT 状态
#[tauri::command]
async fn mqtt_status(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let status = state.mqtt.get_status().await;
    match status {
        MqttStatus::Connected => Ok("connected".to_string()),
        MqttStatus::Connecting => Ok("connecting".to_string()),
        MqttStatus::Disconnected => Ok("disconnected".to_string()),
        MqttStatus::Error(e) => Ok(format!("error:{}", e)),
    }
}

// ─── App Entry ─────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // 初始化 SQLite 数据库
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("获取数据目录失败: {}", e))?;

            let db = storage::Database::init(&app_data_dir)
                .map_err(|e| format!("数据库初始化失败: {}", e))?;

            // 从 SQLite 加载已保存的设备
            let saved = db.load_devices();
            let manager = DeviceManager::new();
            if !saved.is_empty() {
                manager.restore_devices(saved);
                log::info!("已从 SQLite 恢复设备列表");
            }

            let mqtt = Arc::new(MqttManager::new());

            app.manage(AppState { manager, db, mqtt });

            // 后台 ADB 设备监听线程：每 2 秒检测设备变化
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut last_snapshot: Vec<String> = Vec::new();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    // 运行 adb devices 获取当前设备列表
                    let output = match std::process::Command::new("adb").arg("devices").output() {
                        Ok(o) => o,
                        Err(_) => continue,
                    };
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let mut current: Vec<String> = stdout
                        .lines()
                        .skip(1) // 跳过 "List of devices attached"
                        .filter_map(|line| {
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            if parts.len() >= 2 {
                                Some(format!("{}:{}", parts[0], parts[1]))
                            } else {
                                None
                            }
                        })
                        .collect();
                    current.sort();

                    if current != last_snapshot {
                        last_snapshot = current;
                        let _ = handle.emit("devices-changed", ());
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            add_device,
            remove_device,
            list_devices,
            execute_shell,
            get_device_info,
            install_apk,
            reboot_device,
            push_file,
            pull_file,
            remove_all_devices,
            get_settings,
            save_settings,
            mqtt_connect,
            mqtt_disconnect,
            mqtt_subscribe,
            mqtt_publish,
            mqtt_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
