mod connection;
pub mod constants;
mod mqtt;
mod storage;
mod task_engine;
mod task_provider;

use connection::DeviceManager;
use connection::ShellResult;
use mqtt::{MqttConfig, MqttManager, MqttStatus};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use storage::{DailyStatRow, DailySummary, DeviceRow, TaskRunStats};
use task_engine::TaskEngine;
use task_provider::Task;
use tauri::{Emitter, Manager};

// ─── State ─────────────────────────────────────────────────────

struct AppState {
    db: Arc<storage::Database>,
    mqtt: Arc<MqttManager>,
    engine: Arc<TaskEngine>,
}

// FIX #1: 全局线程计数器（限制并发属性获取线程数）
static PROP_FETCH_THREADS: AtomicUsize = AtomicUsize::new(0);

// ─── Tauri Commands ────────────────────────────────────────────

/// FIX #3: 添加设备（async，ADB 连接放 spawn_blocking）
#[tauri::command]
async fn add_device(
    address: String,
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if state.db.device_exists(&address) {
        return Err(format!("设备 {} 已存在", address));
    }
    let entry = DeviceManager::build_wifi_entry(&address, &name)?;

    // ADB 连接操作放入阻塞线程池
    let addr = address.clone();
    let _ =
        tokio::task::spawn_blocking(move || DeviceManager::new().connect_wifi_via_adb(&addr)).await;

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
        state: constants::device_state::OFFLINE.to_string(),
        model: constants::device_state::UNKNOWN.to_string(),
        brand: constants::device_state::UNKNOWN.to_string(),
        android_version: constants::device_state::UNKNOWN.to_string(),
        sdk_version: constants::device_state::UNKNOWN.to_string(),
        display_resolution: constants::device_state::UNKNOWN.to_string(),
        battery_level: 0,
        battery_temperature: 0.0,
        is_flagged: false,
        updated_at: now,
    });

    Ok(format!("设备 {} 已添加", entry.serial))
}

/// 移除设备
#[tauri::command]
fn remove_device(serial: String, state: tauri::State<'_, AppState>) -> Result<String, String> {
    DeviceManager::disconnect_wifi(&serial);
    state.db.delete_device(&serial);
    Ok(format!("设备 {} 已移除", serial))
}

/// 列出所有设备
#[tauri::command]
fn list_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceRow>, String> {
    Ok(state.db.load_all_devices())
}

/// FIX #3: 远程 Shell（async + spawn_blocking）
#[tauri::command]
async fn execute_shell(serial: String, command: String) -> Result<ShellResult, String> {
    tokio::task::spawn_blocking(move || DeviceManager::new().execute_shell(&serial, &command))
        .await
        .map_err(|e| format!("执行失败: {}", e))
}

/// 获取设备详细信息
#[tauri::command]
fn get_device_info(serial: String, state: tauri::State<'_, AppState>) -> Result<DeviceRow, String> {
    state.db.get_device_by_serial(&serial).ok_or_else(|| format!("设备 {} 不存在", serial))
}

/// FIX #3: 安装 APK（async + spawn_blocking）
#[tauri::command]
async fn install_apk(serial: String, apk_path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || DeviceManager::new().install_apk(&serial, &apk_path))
        .await
        .map_err(|e| format!("执行失败: {}", e))?
}

/// FIX #3: 重启设备（async + spawn_blocking）
#[tauri::command]
async fn reboot_device(serial: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || DeviceManager::new().reboot_device(&serial))
        .await
        .map_err(|e| format!("执行失败: {}", e))?
}

/// FIX #3: 推送文件（async + spawn_blocking）
#[tauri::command]
async fn push_file(
    serial: String,
    local_path: String,
    remote_path: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        DeviceManager::new().push_file(&serial, &local_path, &remote_path)
    })
    .await
    .map_err(|e| format!("执行失败: {}", e))?
}

/// FIX #3: 拉取文件（async + spawn_blocking）
#[tauri::command]
async fn pull_file(
    serial: String,
    remote_path: String,
    local_path: String,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        DeviceManager::new().pull_file(&serial, &remote_path, &local_path)
    })
    .await
    .map_err(|e| format!("执行失败: {}", e))?
}

// ─── Settings Commands ─────────────────────────────────────────

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

#[tauri::command]
fn save_settings(
    settings: serde_json::Value,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if let Some(obj) = settings.as_object() {
        for (key, value) in obj {
            if !constants::settings::ALLOWED_KEYS.contains(&key.as_str()) {
                return Err(format!("不允许的设置项: {}", key));
            }
            let v = value.as_str().unwrap_or("");
            state.db.set_setting(key, v);
        }
    }
    Ok("设置已保存".to_string())
}

// ─── MQTT Commands ─────────────────────────────────────────────

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

#[tauri::command]
async fn mqtt_disconnect(state: tauri::State<'_, AppState>) -> Result<String, String> {
    state.mqtt.disconnect().await
}

#[tauri::command]
async fn mqtt_subscribe(
    topic: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.mqtt.subscribe(&topic).await
}

#[tauri::command]
async fn mqtt_publish(
    topic: String,
    payload: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.mqtt.publish(&topic, &payload).await
}

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

#[tauri::command]
fn list_tasks(state: tauri::State<'_, AppState>) -> Vec<Task> {
    task_provider::load_tasks(&state.db)
}

#[tauri::command]
fn get_task_detail(task_id: String, state: tauri::State<'_, AppState>) -> Result<Task, String> {
    task_provider::load_task_by_id(&state.db, &task_id)
        .ok_or_else(|| format!("任务 {} 不存在", task_id))
}

#[tauri::command]
fn get_daily_stats(
    device_serial: String,
    run_date: String,
    state: tauri::State<'_, AppState>,
) -> Vec<DailyStatRow> {
    state.db.query_daily_stats(&device_serial, &run_date)
}

#[tauri::command]
fn get_daily_summary(run_date: String, state: tauri::State<'_, AppState>) -> DailySummary {
    state.db.query_daily_summary(&run_date)
}

#[tauri::command]
fn get_task_run_stats(task_id: String, state: tauri::State<'_, AppState>) -> TaskRunStats {
    state.db.query_task_run_stats(&task_id)
}

#[tauri::command]
fn clear_task_progress(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.db.clear_task_progress(&task_id);
    state.db.delete_task_state(&task_id);
    Ok("ok".to_string())
}

// ─── 后台监控线程 ──────────────────────────────────────────────

const BATCH_PROPS_CMD: &str = "echo \"__MODEL__=$(getprop ro.product.model)\" && \
    echo \"__BRAND__=$(getprop ro.product.brand)\" && \
    echo \"__ANDROID__=$(getprop ro.build.version.release)\" && \
    echo \"__SDK__=$(getprop ro.build.version.sdk)\" && \
    echo \"__SERIAL__=$(getprop ro.serialno)\" && \
    wm size && \
    dumpsys battery";

fn get_tagged_field(raw: &str, tag: &str) -> String {
    raw.lines()
        .find(|l| l.starts_with(tag))
        .map(|l| l[tag.len()..].trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| constants::device_state::UNKNOWN.to_string())
}

fn parse_battery_field(raw: &str, field: &str) -> Option<i32> {
    raw.lines()
        .find(|l| l.trim().starts_with(&format!("{}: ", field)))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
}

fn fetch_device_row(serial: &str, state: &str) -> DeviceRow {
    let device_type = if serial.contains(':') { "wifi" } else { "usb" };
    let address = if device_type == "wifi" { Some(serial.to_string()) } else { None };

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
    let hw_serial = if hw_serial_raw == constants::device_state::UNKNOWN {
        serial.to_string()
    } else {
        hw_serial_raw
    };

    let display_resolution = raw
        .lines()
        .find(|l| l.contains("Physical size"))
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| constants::device_state::UNKNOWN.to_string());

    let battery_level = parse_battery_field(&raw, "level").unwrap_or(0);
    let battery_temp_raw = parse_battery_field(&raw, "temperature").unwrap_or(250);
    let battery_temperature = battery_temp_raw as f64 / 10.0;

    let name =
        if brand != constants::device_state::UNKNOWN && model != constants::device_state::UNKNOWN {
            format!("{} {}", brand, model)
        } else if model != constants::device_state::UNKNOWN {
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
        is_flagged: false,
        updated_at: now,
    }
}

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

fn device_state_str(state: &adb_client::server::DeviceState) -> &'static str {
    use adb_client::server::DeviceState;
    match state {
        DeviceState::Device => constants::device_state::DEVICE,
        DeviceState::Offline => constants::device_state::OFFLINE,
        DeviceState::Unauthorized => "Unauthorized",
        _ => constants::device_state::OFFLINE,
    }
}

/// 启动后台设备监控
fn spawn_device_monitor(handle: tauri::AppHandle, db: Arc<storage::Database>) {
    let db_track = Arc::clone(&db);
    let handle_track = handle.clone();

    // ── 线程 1: track_devices ──
    std::thread::spawn(move || {
        loop {
            let addr = std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(127, 0, 0, 1), 5037);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut server = adb_client::server::ADBServer::new(addr);
                let db_cb = Arc::clone(&db_track);
                let handle_cb = handle_track.clone();

                // FIX #7: 缓存 TTL 从 300ms → 3s
                let devices_cache: std::sync::Mutex<(std::time::Instant, Vec<String>)> =
                    std::sync::Mutex::new((
                        std::time::Instant::now() - std::time::Duration::from_secs(1),
                        Vec::new(),
                    ));

                server.track_devices(move |device| {
                    let serial = device.identifier.clone();
                    let state = device_state_str(&device.state);

                    // 缓存刷新时批量标记离线设备（不再每个事件都执行）
                    {
                        let mut cache = devices_cache.lock().unwrap();
                        if cache.0.elapsed()
                            > std::time::Duration::from_millis(
                                constants::timing::DEVICE_CACHE_TTL_MS,
                            )
                        {
                            let fresh_addr = std::net::SocketAddrV4::new(
                                std::net::Ipv4Addr::new(127, 0, 0, 1),
                                5037,
                            );
                            let mut fresh_server = adb_client::server::ADBServer::new(fresh_addr);
                            let serials: Vec<String> = fresh_server
                                .devices()
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|d| {
                                    matches!(d.state, adb_client::server::DeviceState::Device)
                                })
                                .map(|d| d.identifier)
                                .collect();
                            *cache = (std::time::Instant::now(), serials);

                            // 只在缓存刷新时才执行 mark_offline_except
                            let online_refs: Vec<&str> =
                                cache.1.iter().map(|s| s.as_str()).collect();
                            db_cb.mark_offline_except(&online_refs);
                        }
                    }

                    if !db_cb.device_exists(&serial)
                        || (state == "Device" && db_cb.needs_prop_refresh(&serial))
                    {
                        // 原子的 check-and-increment，避免 TOCTOU 竞争
                        let acquired = PROP_FETCH_THREADS.fetch_update(
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                            |current| {
                                if current < constants::limits::MAX_PROP_FETCH_THREADS {
                                    Some(current + 1)
                                } else {
                                    None
                                }
                            },
                        );
                        if acquired.is_ok() {
                            let db_inner = Arc::clone(&db_cb);
                            let handle_inner = handle_cb.clone();
                            std::thread::spawn(move || {
                                let row = fetch_device_row(&serial, state);
                                // 如果设备在属性获取期间已被标记为 Offline，丢弃本次更新
                                if let Some(current) = db_inner.get_device_by_serial(&row.serial) {
                                    if current.state == constants::device_state::OFFLINE
                                        && row.state == constants::device_state::DEVICE
                                    {
                                        PROP_FETCH_THREADS.fetch_sub(1, Ordering::Relaxed);
                                        return;
                                    }
                                }
                                db_inner.upsert_device(&row);
                                let _ = handle_inner.emit("devices-changed", ());
                                PROP_FETCH_THREADS.fetch_sub(1, Ordering::Relaxed);
                            });
                        } else {
                            // 超过线程限制：仅更新状态，跳过属性获取
                            db_cb.update_device_state(&serial, state);
                        }
                    } else {
                        db_cb.update_device_state(&serial, state);
                    }

                    let _ = handle_cb.emit("devices-changed", ());
                    Ok(())
                })
            }));

            if let Err(_) = result {
                eprintln!(
                    "[monitor] track_devices panic, {}s 后重连...",
                    constants::timing::ADB_RECONNECT_WAIT_SECS
                );
            } else {
                eprintln!(
                    "[monitor] track_devices 连接断开, {}s 后重连...",
                    constants::timing::ADB_RECONNECT_WAIT_SECS
                );
            }
            std::thread::sleep(std::time::Duration::from_secs(
                constants::timing::ADB_RECONNECT_WAIT_SECS,
            ));
        }
    });

    // ── 线程 2: 电池/温度定时刷新 ──
    let db_battery = Arc::clone(&db);
    let handle_battery = handle.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(
                constants::timing::BATTERY_REFRESH_INTERVAL_SECS,
            ));

            let devices = db_battery.load_all_devices();
            let online_devices: Vec<&DeviceRow> =
                devices.iter().filter(|dev| dev.state == "Device").collect();

            if online_devices.is_empty() {
                continue;
            }

            // FIX #9: 限制电池刷新并发线程数
            let max_threads =
                constants::limits::MAX_BATTERY_REFRESH_THREADS.min(online_devices.len());
            let changed = std::sync::atomic::AtomicBool::new(false);

            // 分批处理，每批最多 max_threads 个
            for chunk in online_devices.chunks(max_threads) {
                std::thread::scope(|s| {
                    for dev in chunk {
                        let serial = &dev.serial;
                        let db_ref = &db_battery;
                        let changed_ref = &changed;
                        s.spawn(move || {
                            if refresh_battery(serial, db_ref) {
                                changed_ref.store(true, Ordering::Relaxed);
                            }
                        });
                    }
                });
            }

            if changed.load(Ordering::Relaxed) {
                let _ = handle_battery.emit("devices-changed", ());
            }
        }
    });
}

// ─── Engine Commands ───────────────────────────────────────────

#[tauri::command]
async fn engine_get_tasks(state: tauri::State<'_, AppState>) -> Result<Vec<Task>, String> {
    Ok(state.engine.get_tasks().await)
}

#[tauri::command]
async fn engine_start_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine.start_task(&task_id).await?;
    Ok("ok".into())
}

#[tauri::command]
async fn engine_pause_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine.pause_task(&task_id).await?;
    Ok("ok".into())
}

#[tauri::command]
async fn engine_resume_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine.resume_task(&task_id).await?;
    Ok("ok".into())
}

#[tauri::command]
async fn engine_stop_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine.stop_task(&task_id).await?;
    Ok("ok".into())
}

#[tauri::command]
async fn engine_retry_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine.retry_task(&task_id).await?;
    Ok("ok".into())
}

#[tauri::command]
async fn engine_get_ready_serials(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    Ok(state.engine.get_ready_serials().await)
}

#[tauri::command]
async fn engine_release_offline(
    online_serials: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<u32, String> {
    Ok(state.engine.release_offline_devices(&online_serials).await)
}

#[tauri::command]
async fn engine_reorder_cities(
    task_id: String,
    new_order: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.engine.reorder_cities(&task_id, new_order).await
}

/// 标记设备为风控
#[tauri::command]
fn flag_device(
    serial: String,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    state.db.flag_device(&serial);
    let _ = app.emit("devices-changed", ());
    Ok(format!("设备 {} 已标记风控", serial))
}

/// 解除设备风控标记
#[tauri::command]
fn unflag_device(
    serial: String,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    state.db.unflag_device(&serial);
    let _ = app.emit("devices-changed", ());
    Ok(format!("设备 {} 已解除风控标记", serial))
}

// ─── App Entry ─────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .setup(|app| {
            let app_data_dir =
                app.path().app_data_dir().map_err(|e| format!("获取数据目录失败: {}", e))?;

            let db = Arc::new(
                storage::Database::init(&app_data_dir)
                    .map_err(|e| format!("数据库初始化失败: {}", e))?,
            );

            task_provider::sync_task_cache(&db);

            // 清理上次异常退出的孤儿 run 记录
            db.cleanup_orphan_runs();

            // 启动时重置所有设备为 Offline，等待 track_devices 重新探测真实状态
            db.mark_offline_except(&[]);

            // 清理上次运行残留的 EXECUTING 状态和设备绑定
            db.cleanup_stale_assignments();

            let mqtt = Arc::new(MqttManager::new());
            let engine = TaskEngine::new(Arc::clone(&db), app.handle().clone());

            spawn_device_monitor(app.handle().clone(), Arc::clone(&db));

            app.manage(AppState { db, mqtt, engine });

            Ok(())
        })
        // FIX #12: 移除旧的 record_keyword_complete, start_task_run,
        // finish_task_run, save_task_state, delete_task_state 命令
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
            get_daily_stats,
            get_daily_summary,
            clear_task_progress,
            get_task_run_stats,
            engine_get_tasks,
            engine_start_task,
            engine_pause_task,
            engine_resume_task,
            engine_stop_task,
            engine_retry_task,
            engine_get_ready_serials,
            engine_release_offline,
            engine_reorder_cities,
            flag_device,
            unflag_device,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
