mod connection;
mod mqtt;
mod storage;
mod task_provider;

use connection::{DeviceManager, ShellResult};
use mqtt::{MqttConfig, MqttManager, MqttStatus};
use std::sync::Arc;
use storage::{DailyStatRow, DailySummary, DeviceRow};
use task_provider::Task;
use tauri::{Emitter, Manager};

// ─── State ─────────────────────────────────────────────────────

struct AppState {
    manager: DeviceManager,
    db: Arc<storage::Database>,
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

    // 写入新 DB
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    state.db.upsert_device(&DeviceRow {
        serial: entry.serial.clone(),
        hw_serial: entry.serial.clone(),
        name: entry.name.clone(),
        device_type: match entry.device_type {
            connection::DeviceType::Usb => "usb".to_string(),
            connection::DeviceType::Wifi => "wifi".to_string(),
        },
        address: entry.address.clone(),
        state: "Offline".to_string(), // 后台线程会自动检测并更新为 Device
        model: "unknown".to_string(),
        brand: "unknown".to_string(),
        android_version: "unknown".to_string(),
        sdk_version: "unknown".to_string(),
        display_resolution: "unknown".to_string(),
        battery_level: 0,
        battery_temperature: 0.0,
        updated_at: now,
    });

    Ok(format!("设备 {} 已添加", entry.serial))
}

/// 移除设备（同时断开 WiFi 连接）
#[tauri::command]
fn remove_device(serial: String, state: tauri::State<'_, AppState>) -> Result<String, String> {
    state.manager.remove_device_and_disconnect(&serial)?;
    state.db.delete_device(&serial);
    Ok(format!("设备 {} 已移除", serial))
}

/// 列出所有设备（直接查 DB，瞬时返回）
#[tauri::command]
fn list_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceRow>, String> {
    Ok(state.db.load_all_devices())
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

/// 获取设备详细信息（查 DB，直接返回 DeviceRow）
#[tauri::command]
fn get_device_info(serial: String, state: tauri::State<'_, AppState>) -> Result<DeviceRow, String> {
    let devices = state.db.load_all_devices();
    devices
        .into_iter()
        .find(|d| d.serial == serial)
        .ok_or_else(|| format!("设备 {} 不存在", serial))
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

// ─── Settings Commands ─────────────────────────────────────────

/// 获取设置
#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> serde_json::Value {
    let mqtt_host = state.db.get_setting("mqtt_host").unwrap_or_default();
    let mqtt_port = state.db.get_setting("mqtt_port").unwrap_or_else(|| "1883".to_string());
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
    let host = state.db.get_setting("mqtt_host").unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = state.db.get_setting("mqtt_port").and_then(|s| s.parse().ok()).unwrap_or(1883);
    let client_id = state
        .db
        .get_setting("mqtt_client_id")
        .unwrap_or_else(|| format!("automatex-{}", std::process::id()));
    let username = state.db.get_setting("mqtt_username").filter(|s| !s.is_empty());
    let password = state.db.get_setting("mqtt_password").filter(|s| !s.is_empty());

    let config = MqttConfig { broker_host: host, broker_port: port, client_id, username, password };

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

// ─── Task Commands ─────────────────────────────────────────────

/// 获取任务列表（从 Mock JSON 加载 + DB 进度合并）
#[tauri::command]
fn list_tasks(state: tauri::State<'_, AppState>) -> Vec<Task> {
    task_provider::load_tasks(&state.db)
}

/// 获取单个任务详情
#[tauri::command]
fn get_task_detail(task_id: String, state: tauri::State<'_, AppState>) -> Result<Task, String> {
    let tasks = task_provider::load_tasks(&state.db);
    tasks.into_iter().find(|t| t.id == task_id).ok_or_else(|| format!("任务 {} 不存在", task_id))
}

/// 记录关键词完成
#[tauri::command]
fn record_keyword_complete(
    task_id: String,
    city_name: String,
    keyword_name: String,
    device_serial: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.db.record_keyword_done(&task_id, &city_name, &keyword_name, &device_serial);
    Ok("ok".to_string())
}

/// 开始任务执行记录
#[tauri::command]
fn start_task_run(
    task_id: String,
    device_serial: String,
    state: tauri::State<'_, AppState>,
) -> i64 {
    state.db.start_task_run(&task_id, &device_serial)
}

/// 结束任务执行记录
#[tauri::command]
fn finish_task_run(
    task_id: String,
    started_at: i64,
    status: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.db.finish_task_run(&task_id, started_at, &status);
    Ok("ok".to_string())
}

/// 查询按天统计
#[tauri::command]
fn get_daily_stats(
    device_serial: String,
    run_date: String,
    state: tauri::State<'_, AppState>,
) -> Vec<DailyStatRow> {
    state.db.query_daily_stats(&device_serial, &run_date)
}

/// 查询某天汇总
#[tauri::command]
fn get_daily_summary(run_date: String, state: tauri::State<'_, AppState>) -> DailySummary {
    state.db.query_daily_summary(&run_date)
}

/// 清除任务进度（重跑）
#[tauri::command]
fn clear_task_progress(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.db.clear_task_progress(&task_id);
    Ok("ok".to_string())
}

// ─── 后台监控线程 ──────────────────────────────────────────────

/// 批量获取设备属性的 shell 命令
const BATCH_PROPS_CMD: &str = "echo \"__MODEL__=$(getprop ro.product.model)\" && \
    echo \"__BRAND__=$(getprop ro.product.brand)\" && \
    echo \"__ANDROID__=$(getprop ro.build.version.release)\" && \
    echo \"__SDK__=$(getprop ro.build.version.sdk)\" && \
    echo \"__SERIAL__=$(getprop ro.serialno)\" && \
    wm size && \
    dumpsys battery";

/// 从批量输出中提取带标签的字段
fn get_tagged_field(raw: &str, tag: &str) -> String {
    raw.lines()
        .find(|l| l.starts_with(tag))
        .map(|l| l[tag.len()..].trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 解析 dumpsys battery 中的字段
fn parse_battery_field(raw: &str, field: &str) -> Option<i32> {
    raw.lines()
        .find(|l| l.trim().starts_with(&format!("{}: ", field)))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
}

/// 获取设备完整属性并构建 DeviceRow
fn fetch_device_row(serial: &str, state: &str) -> DeviceRow {
    let device_type = if serial.contains(':') { "wifi" } else { "usb" };
    let address = if device_type == "wifi" { Some(serial.to_string()) } else { None };

    // 批量 adb shell（1 次调用获取所有属性）
    let raw = connection::adb_command()
        .args(["-s", serial, "shell", BATCH_PROPS_CMD])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let model = get_tagged_field(&raw, "__MODEL__=");
    let brand = get_tagged_field(&raw, "__BRAND__=");
    let android_version = get_tagged_field(&raw, "__ANDROID__=");
    let sdk_version = get_tagged_field(&raw, "__SDK__=");
    let hw_serial_raw = get_tagged_field(&raw, "__SERIAL__=");
    let hw_serial = if hw_serial_raw == "unknown" { serial.to_string() } else { hw_serial_raw };

    let display_resolution = raw
        .lines()
        .find(|l| l.contains("Physical size"))
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let battery_level = parse_battery_field(&raw, "level").unwrap_or(0);
    let battery_temp_raw = parse_battery_field(&raw, "temperature").unwrap_or(250);
    let battery_temperature = battery_temp_raw as f64 / 10.0;

    // 查找设备名称（优先用 brand + model）
    let name = if brand != "unknown" && model != "unknown" {
        format!("{} {}", brand, model)
    } else if model != "unknown" {
        model.clone()
    } else {
        serial.to_string()
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    DeviceRow {
        serial: serial.to_string(),
        hw_serial,
        name,
        device_type: device_type.to_string(),
        address,
        state: state.to_string(),
        model,
        brand,
        android_version,
        sdk_version,
        display_resolution,
        battery_level,
        battery_temperature,
        updated_at: now,
    }
}

/// 仅刷新电量和温度
fn refresh_battery(serial: &str, db: &storage::Database) -> bool {
    let raw = connection::adb_command()
        .args(["-s", serial, "shell", "dumpsys battery"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let battery_level = parse_battery_field(&raw, "level").unwrap_or(-1);
    let battery_temp_raw = parse_battery_field(&raw, "temperature").unwrap_or(-1);

    if battery_level >= 0 && battery_temp_raw >= 0 {
        let battery_temperature = battery_temp_raw as f64 / 10.0;
        db.update_device_props(serial, battery_level, battery_temperature);
        true
    } else {
        false
    }
}

/// 将 DeviceState 转为应用层状态字符串
fn device_state_str(state: &adb_client::server::DeviceState) -> &'static str {
    use adb_client::server::DeviceState;
    match state {
        DeviceState::Device => "Device",
        DeviceState::Offline => "Offline",
        DeviceState::Unauthorized => "Unauthorized",
        _ => "Offline",
    }
}

/// 启动后台设备监控（使用 adb_client track_devices 推送模式）
fn spawn_device_monitor(handle: tauri::AppHandle, db: Arc<storage::Database>) {
    let db_track = Arc::clone(&db);
    let handle_track = handle.clone();

    // ── 线程 1: track_devices（设备上下线推送）──
    std::thread::spawn(move || {
        loop {
            // 使用 ADBServer::track_devices 维持 TCP 长连接
            let addr = std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(127, 0, 0, 1), 5037);

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut server = adb_client::server::ADBServer::new(addr);

                // 用 Mutex 收集每批回调的设备列表
                let batch: std::sync::Mutex<Vec<(String, String)>> =
                    std::sync::Mutex::new(Vec::new());
                let batch_changed = std::sync::atomic::AtomicBool::new(false);

                // track_devices 本身是阻塞 loop，回调在同一线程执行
                // 每次 ADB 服务器推送一个事件时，回调会被调用多次（每台设备一次）
                // 事件之间的分界是外层 length 读取（在 track_devices 内部处理）
                // 由于我们无法精确区分批界，每个回调都做一次全量处理
                let db_cb = Arc::clone(&db_track);
                let handle_cb = handle_track.clone();

                server.track_devices(move |device| {
                    let serial = device.identifier.clone();
                    let state = device_state_str(&device.state);

                    // 收集到 batch
                    {
                        let mut b = batch.lock().unwrap();
                        // 如果同一 serial 已存在，更新状态
                        if let Some(existing) = b.iter_mut().find(|(s, _)| s == &serial) {
                            existing.1 = state.to_string();
                        } else {
                            b.push((serial.clone(), state.to_string()));
                        }
                        batch_changed.store(true, std::sync::atomic::Ordering::Relaxed);
                    }

                    // 处理当前已知的所有设备
                    let devices: Vec<(String, String)> = batch.lock().unwrap().clone();

                    // 获取在线设备列表
                    let online_serials: Vec<&str> = devices
                        .iter()
                        .filter(|(_, s)| s == "Device")
                        .map(|(serial, _)| serial.as_str())
                        .collect();

                    // 将不在列表中的设备标记为 Offline
                    db_cb.mark_offline_except(&online_serials);

                    // 处理每台设备
                    for (serial, state) in &devices {
                        if !db_cb.device_exists(serial)
                            || (state == "Device" && db_cb.needs_prop_refresh(serial))
                        {
                            // 新设备或属性未初始化：立即获取全部属性
                            let row = fetch_device_row(serial, state);
                            db_cb.upsert_device(&row);
                        } else {
                            // 已有设备：仅更新状态
                            db_cb.update_device_state(serial, state);
                        }
                    }

                    // 通知前端
                    let _ = handle_cb.emit("devices-changed", ());
                    Ok(())
                })
            }));

            // track_devices 断开时（ADB Server 重启等），自动重连
            if let Err(_) = result {
                eprintln!("[monitor] track_devices panic, 5s 后重连...");
            } else {
                eprintln!("[monitor] track_devices 连接断开, 5s 后重连...");
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });

    // ── 线程 2: 电池/温度定时刷新（每 20s）──
    let db_battery = Arc::clone(&db);
    let handle_battery = handle.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(20));

            // 从 DB 中读取当前在线设备
            let devices = db_battery.load_all_devices();
            let mut changed = false;
            for dev in &devices {
                if dev.state == "Device" {
                    if refresh_battery(&dev.serial, &db_battery) {
                        changed = true;
                    }
                }
            }
            if changed {
                let _ = handle_battery.emit("devices-changed", ());
            }
        }
    });
}

// ─── App Entry ─────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // 初始化 SQLite 数据库
            let app_data_dir =
                app.path().app_data_dir().map_err(|e| format!("获取数据目录失败: {}", e))?;

            let db = Arc::new(
                storage::Database::init(&app_data_dir)
                    .map_err(|e| format!("数据库初始化失败: {}", e))?,
            );

            let manager = DeviceManager::new();
            let mqtt = Arc::new(MqttManager::new());

            // 启动后台设备监控线程
            spawn_device_monitor(app.handle().clone(), Arc::clone(&db));

            app.manage(AppState { manager, db, mqtt });

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
            get_settings,
            save_settings,
            mqtt_connect,
            mqtt_disconnect,
            mqtt_subscribe,
            mqtt_publish,
            mqtt_status,
            list_tasks,
            get_task_detail,
            record_keyword_complete,
            start_task_run,
            finish_task_run,
            get_daily_stats,
            get_daily_summary,
            clear_task_progress,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
