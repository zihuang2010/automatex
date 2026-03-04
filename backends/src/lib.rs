mod connection;
pub mod constants;
mod http_client;
mod mqtt;
mod storage;
mod task_engine;
mod task_provider;

use connection::DeviceManager;
use connection::ShellResult;
use mqtt::{MqttConfig, MqttManager, MqttStatus};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use storage::{DailyStatRow, DailySummary, DeviceRow, TaskRunStats};
use task_engine::TaskEngine;
use task_provider::Task;
use tauri::{Emitter, Listener, Manager};

/// 基于机器指纹生成稳定唯一的 clientId
/// 采集 hostname + username + OS + arch，hash 后生成 16 位 hex 标识
fn generate_machine_client_id() -> String {
    use std::hash::{Hash, Hasher};

    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| {
            // macOS/Linux fallback
            std::process::Command::new("hostname")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|_| "unknown-host".to_string());

    let username = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown-user".to_string());

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let fingerprint = format!("{}|{}|{}|{}", hostname, username, os, arch);

    // 使用两轮不同种子的 hash 来生成 16 位 hex（128 bit 空间）
    let mut hasher1 = std::collections::hash_map::DefaultHasher::new();
    fingerprint.hash(&mut hasher1);
    let h1 = hasher1.finish();

    let mut hasher2 = std::collections::hash_map::DefaultHasher::new();
    format!("salt-v1-{}", fingerprint).hash(&mut hasher2);
    let h2 = hasher2.finish();

    let id = format!("automatex-{:08x}{:08x}", h1 as u32, h2 as u32);
    eprintln!("[client_id] 机器指纹: {} → {}", fingerprint, id);
    id
}

/// 确保 DB 中存在 mqtt_client_id，不存在则基于机器指纹生成并持久化
async fn ensure_client_id(db: &storage::Database) -> String {
    if let Some(existing) = db.get_setting("mqtt_client_id").await {
        if !existing.is_empty() {
            eprintln!("[client_id] 使用已有: {}", existing);
            return existing;
        }
    }
    let id = generate_machine_client_id();
    db.set_setting("mqtt_client_id", &id).await;
    eprintln!("[client_id] 首次生成并持久化: {}", id);
    id
}

/// 跨日检测：比较 last_active_date 与今天，不同则执行完整重置
async fn check_daily_reset(db: &storage::Database) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let last_date = db.get_setting("last_active_date").await.unwrap_or_default();

    if last_date == today {
        eprintln!("[startup] 日期未变 ({}), 跳过跨日重置", today);
        return;
    }

    eprintln!(
        "[startup] 检测到跨日: {} → {}, 执行重置...",
        if last_date.is_empty() { "首次" } else { &last_date },
        today
    );

    // 1. 关闭所有 running 轮次
    db.close_all_running_rounds().await;

    // 2. 重置所有任务状态
    db.daily_reset_tasks().await;

    // 3. 删除所有设备缓存
    db.delete_all_devices().await;

    // 4. 清理已同步的进度（保留 pending 未上报的）
    db.cleanup_synced_progress().await;

    // 5. 清理孤儿 run 记录
    db.cleanup_orphan_runs().await;

    // 6. 更新日期标记
    db.set_setting("last_active_date", &today).await;
    eprintln!("[startup] 跨日重置完成, last_active_date={}", today);
}

/// 启动时自动同步：验证已绑定手机号 → 清理冲突 → 拉取最新任务
async fn startup_sync_tasks(
    db: &Arc<storage::Database>,
    http: &Arc<http_client::HttpClient>,
    engine: &Arc<TaskEngine>,
    client_id: &str,
    app_handle: &tauri::AppHandle,
) {
    use constants::tauri_event;

    // 读取已绑定的手机号
    let synced_phones: Vec<String> =
        serde_json::from_str(&db.get_setting("synced_phones").await.unwrap_or_default())
            .unwrap_or_default();

    if synced_phones.is_empty() {
        eprintln!("[startup] 无已绑定手机号，通知前端跳转绑定页面");
        let _ = app_handle.emit(
            tauri_event::REQUIRE_PHONE_BIND,
            serde_json::json!({
                "reason": "no_phones",
                "message": "请绑定手机号后开始使用"
            }),
        );
        return;
    }

    eprintln!("[startup] 检测到已绑定手机号: {:?}, 验证有效性...", synced_phones);
    let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "syncing");

    // 调用服务端验证绑定（force=false，不抢占）
    let bind_req = http_client::PhoneBindRequest {
        client_id: client_id.to_string(),
        phones: synced_phones.clone(),
        force: false,
    };

    match http.bind_phones(&bind_req).await {
        Ok(bind_resp) => {
            // 处理冲突（被其他客户端抢走的手机号）
            if !bind_resp.conflicts.is_empty() {
                let conflict_phones: Vec<String> =
                    bind_resp.conflicts.iter().map(|c| c.phone.clone()).collect();
                eprintln!("[startup] 检测到异地登录冲突: {:?}, 清理关联任务", conflict_phones);
                engine.handle_phones_unbind(conflict_phones).await;
            }

            let valid_phones = bind_resp.bound;

            if valid_phones.is_empty() {
                // 所有手机号都失效了
                eprintln!("[startup] 所有手机号已失效，通知前端跳转绑定页面");
                db.set_setting("synced_phones", "[]").await;
                let _ = app_handle.emit(
                    tauri_event::REQUIRE_PHONE_BIND,
                    serde_json::json!({
                        "reason": "all_expired",
                        "message": "已绑定的手机号已在其他设备登录，请重新绑定"
                    }),
                );
            } else {
                // 拉取有效手机号的最新任务
                eprintln!("[startup] 有效手机号: {:?}, 拉取最新任务...", valid_phones);

                match http.fetch_tasks_by_phones(client_id, &valid_phones).await {
                    Ok(resp) => {
                        let mut count = 0usize;
                        for (phone, defs) in &resp.phone_tasks {
                            for def in defs {
                                let payload =
                                    serde_json::to_string(&def.cities).unwrap_or_default();
                                db.upsert_task_def(&def.id, &def.name, &payload, 1, phone).await;
                                count += 1;
                            }
                        }
                        // 更新 synced_phones（可能去掉了冲突的）
                        db.set_setting(
                            "synced_phones",
                            &serde_json::to_string(&valid_phones).unwrap_or_default(),
                        )
                        .await;
                        engine.reload_tasks().await;
                        eprintln!(
                            "[startup] 同步完成: {} 个手机号, {} 个任务",
                            valid_phones.len(),
                            count
                        );
                    },
                    Err(e) => {
                        eprintln!("[startup] 拉取任务失败: {}, 使用本地缓存", e);
                    },
                }
            }
        },
        Err(e) => {
            // 网络不可用时静默降级，使用本地缓存
            eprintln!("[startup] 验证绑定失败(网络?): {}, 使用本地缓存", e);
        },
    }

    let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "done");
}

// ─── State ─────────────────────────────────────────────────────

struct AppState {
    db: Arc<storage::Database>,
    mqtt: Arc<MqttManager>,
    engine: Arc<tokio::sync::OnceCell<Arc<TaskEngine>>>,
    http: Arc<tokio::sync::OnceCell<Arc<http_client::HttpClient>>>,
}

impl AppState {
    /// 获取引擎引用（未初始化时返回友好错误）
    fn engine(&self) -> Result<&Arc<TaskEngine>, String> {
        self.engine.get().ok_or_else(|| "引擎正在初始化，请稍后重试".to_string())
    }

    /// 获取 HTTP 客户端引用（未初始化时返回友好错误）
    fn http(&self) -> Result<&Arc<http_client::HttpClient>, String> {
        self.http.get().ok_or_else(|| "HTTP 客户端正在初始化，请稍后重试".to_string())
    }
}

// FIX #1: 全局线程计数器（限制并发属性获取线程数）
static PROP_FETCH_THREADS: AtomicUsize = AtomicUsize::new(0);

// ─── Tauri Commands ────────────────────────────────────────────

#[tauri::command]
async fn add_device(
    address: String,
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if state.db.device_exists(&address).await {
        return Err(format!("设备 {} 已存在", address));
    }
    let entry = DeviceManager::build_wifi_entry(&address, &name)?;

    let addr = address.clone();
    let _ =
        tokio::task::spawn_blocking(move || DeviceManager::new().connect_wifi_via_adb(&addr)).await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    state
        .db
        .upsert_device(&DeviceRow {
            serial: entry.serial.clone(),
            hw_serial: entry.serial.clone(),
            name: entry.name.clone(),
            device_type: match entry.device_type {
                connection::DeviceType::Usb => constants::device_type::USB.to_string(),
                connection::DeviceType::Wifi => constants::device_type::WIFI.to_string(),
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
        })
        .await;

    Ok(format!("设备 {} 已添加", entry.serial))
}

#[tauri::command]
async fn remove_device(
    serial: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    DeviceManager::disconnect_wifi(&serial);
    state.db.delete_device(&serial).await;
    Ok(format!("设备 {} 已移除", serial))
}

#[tauri::command]
async fn list_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceRow>, String> {
    Ok(state.db.load_all_devices().await)
}

#[tauri::command]
async fn execute_shell(serial: String, command: String) -> Result<ShellResult, String> {
    tokio::task::spawn_blocking(move || DeviceManager::new().execute_shell(&serial, &command))
        .await
        .map_err(|e| format!("执行失败: {}", e))
}

#[tauri::command]
async fn get_device_info(
    serial: String,
    state: tauri::State<'_, AppState>,
) -> Result<DeviceRow, String> {
    state
        .db
        .get_device_by_serial(&serial)
        .await
        .ok_or_else(|| format!("设备 {} 不存在", serial))
}

#[tauri::command]
async fn install_apk(serial: String, apk_path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || DeviceManager::new().install_apk(&serial, &apk_path))
        .await
        .map_err(|e| format!("执行失败: {}", e))?
}

#[tauri::command]
async fn reboot_device(serial: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || DeviceManager::new().reboot_device(&serial))
        .await
        .map_err(|e| format!("执行失败: {}", e))?
}

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

fn setting_or(map: &std::collections::HashMap<String, String>, key: &str, default: &str) -> String {
    map.get(key).cloned().unwrap_or_else(|| default.to_string())
}

#[tauri::command]
async fn get_settings(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let s = state.db.get_all_settings().await;

    Ok(serde_json::json!({
        "mqtt_host": setting_or(&s, "mqtt_host", ""),
        "mqtt_port": setting_or(&s, "mqtt_port", "1883"),
        "mqtt_client_id": s.get("mqtt_client_id").cloned()
            .unwrap_or_else(|| format!("automatex-{}", std::process::id())),
        "mqtt_username": setting_or(&s, "mqtt_username", ""),
        "mqtt_password": setting_or(&s, "mqtt_password", ""),
        "synced_phones": setting_or(&s, "synced_phones", "[]"),
        "theme": setting_or(&s, "theme", "dark"),
    }))
}

#[tauri::command]
async fn save_settings(
    settings: serde_json::Value,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if let Some(obj) = settings.as_object() {
        // 先验证所有 key
        for key in obj.keys() {
            if !constants::settings::ALLOWED_KEYS.contains(&key.as_str()) {
                return Err(format!("不允许的设置项: {}", key));
            }
        }
        // 批量写入（单连接内完成）
        let pairs: Vec<(String, String)> = obj
            .iter()
            .map(|(k, v)| {
                let val = match v.as_str() {
                    Some(s) => s.to_string(),
                    None => v.to_string(),
                };
                (k.clone(), val)
            })
            .collect();
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        state.db.set_settings_batch(&refs).await;
    }
    Ok("设置已保存".to_string())
}

// ─── Phone Sync Commands ───────────────────────────────────────

#[tauri::command]
async fn sync_tasks_by_phones(
    phones: Vec<String>,
    force: bool,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let http = state.http()?;
    let engine = state.engine()?;

    let s = state.db.get_all_settings().await;
    let client_id =
        s.get("mqtt_client_id").cloned().unwrap_or_else(|| generate_machine_client_id());

    let bind_req = http_client::PhoneBindRequest {
        client_id: client_id.clone(),
        phones: phones.clone(),
        force,
    };
    let bind_resp = http.bind_phones(&bind_req).await?;

    if !bind_resp.conflicts.is_empty() && !force {
        return Ok(serde_json::json!({
            "status": "conflicts",
            "bound": bind_resp.bound,
            "conflicts": bind_resp.conflicts,
        }));
    }

    let bound_phones = if force { phones.clone() } else { bind_resp.bound };

    // 清理被移除的手机号（防止直接替换手机号时旧 task_defs 残留）
    let old_phones: Vec<String> =
        serde_json::from_str(&state.db.get_setting("synced_phones").await.unwrap_or_default())
            .unwrap_or_default();
    let removed_phones: Vec<String> =
        old_phones.into_iter().filter(|p| !bound_phones.contains(p)).collect();
    if !removed_phones.is_empty() {
        eprintln!("[sync] 检测到被移除的手机号: {:?}，清理旧任务数据", removed_phones);
        engine.handle_phones_unbind(removed_phones).await;
    }

    let resp = http.fetch_tasks_by_phones(&client_id, &bound_phones).await?;

    let mut count = 0usize;
    for (phone, defs) in &resp.phone_tasks {
        for def in defs {
            let payload = serde_json::to_string(&def.cities).unwrap_or_default();
            state.db.upsert_task_def(&def.id, &def.name, &payload, 1, phone).await;
            count += 1;
        }
    }
    state
        .db
        .set_setting("synced_phones", &serde_json::to_string(&bound_phones).unwrap_or_default())
        .await;

    engine.reload_tasks().await;

    eprintln!("[sync] 同步完成: {} 个手机号, {} 个任务", bound_phones.len(), count);

    Ok(serde_json::json!({
        "status": constants::response::OK,
        "phones": bound_phones.len(),
        "tasks": count,
    }))
}

// ─── MQTT Commands ─────────────────────────────────────────────

fn build_mqtt_config_from(s: &std::collections::HashMap<String, String>) -> MqttConfig {
    let host = setting_or(s, "mqtt_host", "127.0.0.1");
    let port: u16 = s.get("mqtt_port").and_then(|v| v.parse().ok()).unwrap_or(30002);
    let client_id = s
        .get("mqtt_client_id")
        .cloned()
        .unwrap_or_else(|| format!("automatex-{}", std::process::id()));
    let username = s.get("mqtt_username").filter(|v| !v.is_empty()).cloned();
    let password = s.get("mqtt_password").filter(|v| !v.is_empty()).cloned();
    MqttConfig { broker_host: host, broker_port: port, client_id, username, password }
}

#[tauri::command]
async fn mqtt_connect(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let s = state.db.get_all_settings().await;
    let config = build_mqtt_config_from(&s);
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
        MqttStatus::Connected => Ok(constants::mqtt_emit_status::CONNECTED.to_string()),
        MqttStatus::Connecting => Ok(constants::mqtt_emit_status::CONNECTING.to_string()),
        MqttStatus::Disconnected => Ok(constants::mqtt_emit_status::DISCONNECTED.to_string()),
        MqttStatus::Error(e) => Ok(format!("error:{}", e)),
    }
}

// ─── Task Commands ─────────────────────────────────────────────

#[tauri::command]
async fn list_tasks(state: tauri::State<'_, AppState>) -> Result<Vec<Task>, String> {
    Ok(task_provider::load_tasks(&state.db).await)
}

#[tauri::command]
async fn get_task_detail(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Task, String> {
    task_provider::load_task_by_id(&state.db, &task_id)
        .await
        .ok_or_else(|| format!("任务 {} 不存在", task_id))
}

#[tauri::command]
async fn get_daily_stats(
    device_serial: String,
    run_date: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<DailyStatRow>, String> {
    Ok(state.db.query_daily_stats(&device_serial, &run_date).await)
}

#[tauri::command]
async fn get_daily_summary(
    run_date: String,
    state: tauri::State<'_, AppState>,
) -> Result<DailySummary, String> {
    Ok(state.db.query_daily_summary(&run_date).await)
}

#[tauri::command]
async fn get_task_run_stats(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<TaskRunStats, String> {
    Ok(state.db.query_task_run_stats(&task_id).await)
}

#[tauri::command]
async fn clear_task_progress(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.db.clear_task_progress(&task_id).await;
    state.db.delete_task_state(&task_id).await;
    Ok(constants::response::OK.to_string())
}

// ─── 后台监控线程 ──────────────────────────────────────────────

const BATCH_PROPS_CMD: &str = "echo \"__MODEL__=$(getprop ro.product.model)\" && \
    echo \"__BRAND__=$(getprop ro.product.brand)\" && \
    echo \"__ANDROID__=$(getprop ro.build.version.release)\" && \
    echo \"__SDK__=$(getprop ro.build.version.sdk)\" && \
    echo \"__SERIAL__=$(getprop ro.serialno)\" && \
    echo \"__BOOTSERIAL__=$(getprop ro.boot.serialno)\" && \
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
    let device_type = if serial.contains(':') {
        constants::device_type::WIFI
    } else {
        constants::device_type::USB
    };
    let address = if device_type == constants::device_type::WIFI {
        Some(serial.to_string())
    } else {
        None
    };

    let raw = connection::run_adb_timed(
        connection::adb_command().args(["-s", serial, "shell", BATCH_PROPS_CMD]),
        constants::timing::ADB_COMMAND_TIMEOUT_SECS,
    )
    .ok()
    .filter(|o| o.status.success())
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    .unwrap_or_default();

    let model = get_tagged_field(&raw, "__MODEL__=");
    let brand = get_tagged_field(&raw, "__BRAND__=");
    let android_version = get_tagged_field(&raw, "__ANDROID__=");
    let sdk_version = get_tagged_field(&raw, "__SDK__=");
    let hw_serial_raw = get_tagged_field(&raw, "__SERIAL__=");
    let hw_serial = if hw_serial_raw != constants::device_state::UNKNOWN {
        hw_serial_raw
    } else {
        let boot_serial = get_tagged_field(&raw, "__BOOTSERIAL__=");
        if boot_serial != constants::device_state::UNKNOWN {
            boot_serial
        } else {
            serial.to_string()
        }
    };

    let display_resolution = raw
        .lines()
        .find(|l| l.contains("Physical size"))
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| constants::device_state::UNKNOWN.to_string());

    let battery_level = parse_battery_field(&raw, "level").unwrap_or(-1);
    let battery_temp_raw = parse_battery_field(&raw, "temperature").unwrap_or(-1);
    let battery_temperature =
        if battery_temp_raw >= 0 { battery_temp_raw as f64 / 10.0 } else { -1.0 };

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

/// 同步封装：在 std::thread 中调用 async DB 方法
fn db_block_on<F, T>(rt: &tokio::runtime::Handle, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    rt.block_on(f)
}

fn device_state_str(state: &adb_client::server::DeviceState) -> &'static str {
    use adb_client::server::DeviceState;
    match state {
        DeviceState::Device => constants::device_state::DEVICE,
        DeviceState::Offline => constants::device_state::OFFLINE,
        DeviceState::Unauthorized => constants::device_state::UNAUTHORIZED,
        _ => constants::device_state::OFFLINE,
    }
}

/// 启动后台设备监控
fn spawn_device_monitor(
    handle: tauri::AppHandle,
    db: Arc<storage::Database>,
    device_ready: Arc<tokio::sync::Notify>,
    rt: tokio::runtime::Handle,
) {
    let db_track = Arc::clone(&db);
    let handle_track = handle.clone();
    let rt_track = rt.clone();

    // ── 线程 1: track_devices ──
    std::thread::spawn(move || loop {
        let addr = std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(127, 0, 0, 1), 5037);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut server = adb_client::server::ADBServer::new(addr);
            let db_cb = Arc::clone(&db_track);
            let handle_cb = handle_track.clone();
            let ready = Arc::clone(&device_ready);
            let rt_cb = rt_track.clone();

            let devices_cache: std::sync::Mutex<(std::time::Instant, Vec<String>)> =
                std::sync::Mutex::new((
                    std::time::Instant::now() - std::time::Duration::from_secs(1),
                    Vec::new(),
                ));

            server.track_devices(move |device| {
                let serial = device.identifier.clone();
                let state = device_state_str(&device.state);

                {
                    let mut cache = devices_cache.lock().unwrap_or_else(|e| e.into_inner());
                    if cache.0.elapsed()
                        > std::time::Duration::from_millis(constants::timing::DEVICE_CACHE_TTL_MS)
                    {
                        let fresh_addr = std::net::SocketAddrV4::new(
                            std::net::Ipv4Addr::new(127, 0, 0, 1),
                            5037,
                        );
                        let mut fresh_server = adb_client::server::ADBServer::new(fresh_addr);
                        match fresh_server.devices() {
                            Ok(devices) => {
                                let serials: Vec<String> = devices
                                    .into_iter()
                                    .filter(|d| {
                                        matches!(d.state, adb_client::server::DeviceState::Device)
                                    })
                                    .map(|d| d.identifier)
                                    .collect();
                                *cache = (std::time::Instant::now(), serials);

                                let online = cache.1.clone();
                                db_block_on(&rt_cb, db_cb.mark_offline_except(online));

                                ready.notify_one();
                            },
                            Err(e) => {
                                eprintln!("[monitor] devices() 查询失败，保留上次缓存: {}", e);
                                cache.0 = std::time::Instant::now();
                            },
                        }
                    }
                }

                if !db_block_on(&rt_cb, db_cb.device_exists(&serial))
                    || (state == constants::device_state::DEVICE
                        && db_block_on(&rt_cb, db_cb.needs_prop_refresh(&serial)))
                {
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
                        let rt_inner = rt_cb.clone();
                        std::thread::spawn(move || {
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    fetch_device_row(&serial, state)
                                }));
                            match result {
                                Ok(row) => {
                                    db_block_on(&rt_inner, db_inner.upsert_device(&row));
                                    let _ = handle_inner
                                        .emit(constants::tauri_event::DEVICES_CHANGED, ());
                                },
                                Err(_) => {
                                    eprintln!("[monitor] fetch_device_row panic: {}", serial);
                                },
                            }
                            PROP_FETCH_THREADS.fetch_sub(1, Ordering::SeqCst);
                        });
                    } else {
                        db_block_on(&rt_cb, db_cb.update_device_state(&serial, state));
                    }
                } else {
                    db_block_on(&rt_cb, db_cb.update_device_state(&serial, state));
                }

                let _ = handle_cb.emit(constants::tauri_event::DEVICES_CHANGED, ());
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
    });

    // ── 线程 2: 电池/温度定时刷新 ──
    let db_battery = Arc::clone(&db);
    let handle_battery = handle.clone();
    let rt_battery = rt;
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(
            constants::timing::BATTERY_REFRESH_INTERVAL_SECS,
        ));

        let devices = db_block_on(&rt_battery, db_battery.load_all_devices());
        let online_devices: Vec<&DeviceRow> = devices
            .iter()
            .filter(|dev| dev.state == constants::device_state::DEVICE)
            .collect();

        if online_devices.is_empty() {
            continue;
        }

        let max_threads = constants::limits::MAX_BATTERY_REFRESH_THREADS.min(online_devices.len());
        let changed = std::sync::atomic::AtomicBool::new(false);

        for chunk in online_devices.chunks(max_threads) {
            std::thread::scope(|s| {
                for dev in chunk {
                    let serial = &dev.serial;
                    let db_ref = &db_battery;
                    let rt_ref = &rt_battery;
                    let changed_ref = &changed;
                    s.spawn(move || {
                        if refresh_battery(serial, db_ref, rt_ref) {
                            changed_ref.store(true, Ordering::Relaxed);
                        }
                    });
                }
            });
        }

        if changed.load(Ordering::Relaxed) {
            let _ = handle_battery.emit(constants::tauri_event::DEVICES_CHANGED, ());
        }

        // WiFi 设备并行重连（避免串行阻塞电池刷新线程）
        let wifi_devices: Vec<String> = devices
            .iter()
            .filter(|d| {
                d.device_type == constants::device_type::WIFI
                    && d.state == constants::device_state::OFFLINE
                    && d.serial.contains(':')
            })
            .map(|d| d.serial.clone())
            .collect();
        if !wifi_devices.is_empty() {
            std::thread::scope(|s| {
                for addr in &wifi_devices {
                    s.spawn(move || {
                        let output = connection::run_adb_timed(
                            connection::adb_command().args(["connect", addr]),
                            constants::timing::WIFI_CONNECT_TIMEOUT_SECS,
                        );
                        match output {
                            Ok(o) if o.status.success() => {
                                let stdout = String::from_utf8_lossy(&o.stdout);
                                if !stdout.contains("failed") {
                                    eprintln!("[wifi-reconnect] 重连成功: {}", addr);
                                }
                            },
                            _ => {},
                        }
                    });
                }
            });
        }
    });
}

fn refresh_battery(serial: &str, db: &storage::Database, rt: &tokio::runtime::Handle) -> bool {
    let raw = connection::run_adb_timed(
        connection::adb_command().args(["-s", serial, "shell", "dumpsys battery"]),
        constants::timing::ADB_COMMAND_TIMEOUT_SECS,
    )
    .ok()
    .filter(|o| o.status.success())
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    .unwrap_or_default();

    let battery_level = parse_battery_field(&raw, "level").unwrap_or(-1);
    let battery_temp_raw = parse_battery_field(&raw, "temperature").unwrap_or(-1);

    if battery_level >= 0 && battery_temp_raw >= 0 {
        let battery_temperature = battery_temp_raw as f64 / 10.0;
        db_block_on(rt, db.update_device_props(serial, battery_level, battery_temperature));
        true
    } else {
        false
    }
}

// ─── Engine Commands ───────────────────────────────────────────

#[tauri::command]
async fn engine_get_tasks(state: tauri::State<'_, AppState>) -> Result<Vec<Task>, String> {
    Ok(state.engine()?.get_tasks().await)
}

#[tauri::command]
async fn engine_start_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.start_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
async fn engine_pause_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.pause_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
async fn engine_resume_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.resume_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
async fn engine_stop_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.stop_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
async fn engine_retry_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.retry_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
async fn engine_get_ready_serials(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    Ok(state.engine()?.get_ready_serials().await)
}

#[tauri::command]
async fn engine_release_offline(
    online_serials: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<u32, String> {
    Ok(state.engine()?.release_offline_devices(&online_serials).await)
}

#[tauri::command]
async fn engine_reorder_cities(
    task_id: String,
    new_order: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.engine()?.reorder_cities(&task_id, new_order).await
}

#[tauri::command]
async fn flag_device(
    serial: String,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    state.db.flag_device(&serial).await;
    let _ = app.emit(constants::tauri_event::DEVICES_CHANGED, ());
    Ok(format!("设备 {} 已标记风控", serial))
}

#[tauri::command]
async fn unflag_device(
    serial: String,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    state.db.unflag_device(&serial).await;
    let _ = app.emit(constants::tauri_event::DEVICES_CHANGED, ());
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

            // Database::init 是同步的（裸 Connection 建表 + 创建 Pool）
            let db = Arc::new(
                storage::Database::init(&app_data_dir)
                    .map_err(|e| format!("数据库初始化失败: {}", e))?,
            );

            let mqtt = Arc::new(MqttManager::new());
            let engine: Arc<tokio::sync::OnceCell<Arc<TaskEngine>>> =
                Arc::new(tokio::sync::OnceCell::new());
            let http: Arc<tokio::sync::OnceCell<Arc<http_client::HttpClient>>> =
                Arc::new(tokio::sync::OnceCell::new());

            let device_ready = Arc::new(tokio::sync::Notify::new());

            // ── 异步初始化（在 tokio runtime 中执行） ──
            {
                let db_init = Arc::clone(&db);
                let engine_cell = Arc::clone(&engine);
                let http_cell = Arc::clone(&http);
                let app_handle = app.handle().clone();
                let device_ready_clone = Arc::clone(&device_ready);
                tauri::async_runtime::spawn(async move {
                    let rt = tokio::runtime::Handle::current();

                    // 异步 DB 清理
                    task_provider::sync_task_cache(&db_init).await;
                    db_init.cleanup_orphan_runs().await;
                    db_init.mark_offline_except(Vec::new()).await;
                    db_init.cleanup_stale_assignments().await;

                    // 确保 clientId 存在（首次启动时基于机器指纹生成）
                    let client_id = ensure_client_id(&db_init).await;

                    // ── 跨日检测 + 重置 ──
                    check_daily_reset(&db_init).await;

                    // 创建 HTTP 客户端
                    let http_base_url =
                        db_init.get_setting("api_base_url").await.unwrap_or_default();
                    let http_client = Arc::new(http_client::HttpClient::new(&http_base_url));
                    let _ = http_cell.set(Arc::clone(&http_client));

                    // 创建引擎
                    let eng = TaskEngine::new(
                        Arc::clone(&db_init),
                        Arc::clone(&http_client),
                        app_handle.clone(),
                    )
                    .await;
                    let _ = engine_cell.set(Arc::clone(&eng));

                    eprintln!("[startup] 异步初始化完成，引擎已就绪");

                    // ── 启动同步：验证账号 + 拉取任务 ──
                    startup_sync_tasks(&db_init, &http_client, &eng, &client_id, &app_handle).await;

                    // ── MQTT 自动连接（独立于设备同步，立即执行） ──
                    {
                        let db_mqtt = Arc::clone(&db_init);
                        let app_mqtt = app_handle.clone();
                        tokio::spawn(async move {
                            let startup_settings = db_mqtt.get_all_settings().await;
                            let has_host = startup_settings.contains_key("mqtt_host");
                            let auto_off = startup_settings
                                .get("mqtt_auto_connect")
                                .map(|v| v == "false")
                                .unwrap_or(false);

                            if has_host && !auto_off {
                                let config = build_mqtt_config_from(&startup_settings);
                                tokio::time::sleep(Duration::from_millis(500)).await;
                                eprintln!(
                                    "[startup] MQTT 自动连接: {}:{}",
                                    config.broker_host, config.broker_port
                                );
                                let mqtt_state = app_mqtt.state::<AppState>();
                                match mqtt_state.mqtt.connect(config, app_mqtt.clone()).await {
                                    Ok(msg) => eprintln!("[startup] {}", msg),
                                    Err(e) => eprintln!("[startup] MQTT 自动连接失败: {}", e),
                                }
                            } else {
                                eprintln!("[startup] MQTT 未配置主机或已禁用自动连接，跳过");
                            }
                        });
                    }

                    // 启动设备监控
                    spawn_device_monitor(
                        app_handle.clone(),
                        Arc::clone(&db_init),
                        Arc::clone(&device_ready_clone),
                        rt.clone(),
                    );

                    // ── 设备归属同步（等待设备就绪后执行） ──
                    eprintln!("[startup] 等待设备就绪...");
                    device_ready_clone.notified().await;
                    eprintln!("[startup] 设备就绪，开始归属同步");

                    let devices = db_init.load_all_devices().await;
                    let online: Vec<http_client::DeviceSyncItem> = devices
                        .iter()
                        .filter(|d| d.state == constants::device_state::DEVICE)
                        .map(|d| http_client::DeviceSyncItem {
                            hw_serial: d.hw_serial.clone(),
                            serial: d.serial.clone(),
                            state: d.state.clone(),
                        })
                        .collect();
                    let offline_local: Vec<String> = devices
                        .iter()
                        .filter(|d| d.state != constants::device_state::DEVICE)
                        .map(|d| d.hw_serial.clone())
                        .collect();

                    let client_id = db_init
                        .get_setting("mqtt_client_id")
                        .await
                        .unwrap_or_else(|| generate_machine_client_id());

                    let req = http_client::DeviceSyncRequest { client_id, online, offline_local };

                    match http_client.device_sync(&req).await {
                        Ok(resp) => {
                            if !resp.to_remove.is_empty() {
                                eprintln!(
                                    "[startup] 清理被其他客户端占用的设备: {:?}",
                                    resp.to_remove
                                );
                                let n = eng.handle_device_kick(resp.to_remove).await;
                                eprintln!("[startup] 已清理 {} 台设备", n);
                                let _ =
                                    app_handle.emit(constants::tauri_event::DEVICES_CHANGED, ());
                            }
                        },
                        Err(e) => {
                            eprintln!("[startup] 设备归属同步失败: {}", e);
                        },
                    }
                });
            }

            // ── MQTT 事件监听 ──
            {
                let engine_kick = Arc::clone(&engine);
                app.listen(constants::tauri_event::MQTT_DEVICE_KICK, move |event| {
                    let engine = Arc::clone(&engine_kick);
                    tauri::async_runtime::spawn(async move {
                        let Some(eng) = engine.get() else {
                            eprintln!("[mqtt-listener] engine 未初始化，跳过 device-kick");
                            return;
                        };
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(event.payload())
                        {
                            if let Some(hw_serials) =
                                val.get("hw_serials").and_then(|v| v.as_array())
                            {
                                let serials: Vec<String> = hw_serials
                                    .iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect();
                                let n = eng.handle_device_kick(serials).await;
                                eprintln!("[mqtt-listener] 踢设备完成: {} 台", n);
                            }
                        }
                    });
                });

                let engine_reload = Arc::clone(&engine);
                app.listen(constants::tauri_event::MQTT_TASK_RELOAD, move |event| {
                    let engine = Arc::clone(&engine_reload);
                    tauri::async_runtime::spawn(async move {
                        let Some(eng) = engine.get() else {
                            eprintln!("[mqtt-listener] engine 未初始化，跳过 task-reload");
                            return;
                        };
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(event.payload())
                        {
                            let action = val.get("action").and_then(|v| v.as_str()).unwrap_or("");
                            let task_id = val.get("task_id").and_then(|v| v.as_str());
                            eng.handle_task_reload(action, task_id).await;
                        }
                    });
                });
            }

            // ── MQTT 手机号解绑监听 ──
            {
                let engine_unbind = Arc::clone(&engine);
                app.listen(constants::tauri_event::MQTT_PHONES_UNBIND, move |event| {
                    let engine = Arc::clone(&engine_unbind);
                    tauri::async_runtime::spawn(async move {
                        let Some(eng) = engine.get() else {
                            eprintln!("[mqtt-listener] engine 未初始化，跳过 phones-unbind");
                            return;
                        };
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(event.payload())
                        {
                            if let Some(phones) = val.get("phones").and_then(|v| v.as_array()) {
                                let phone_list: Vec<String> = phones
                                    .iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect();
                                let n = eng.handle_phones_unbind(phone_list).await;
                                eprintln!("[mqtt-listener] 手机号解绑完成: {} 个任务已移除", n);
                            }
                        }
                    });
                });
            }

            // ── MQTT 心跳定时器 ──
            {
                let mqtt_hb = Arc::clone(&mqtt);
                let db_hb = Arc::clone(&db);
                let engine_hb = Arc::clone(&engine);
                tauri::async_runtime::spawn(async move {
                    // 等引擎初始化完成（轮询等待）
                    loop {
                        if engine_hb.get().is_some() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    let eng = engine_hb.get().unwrap();
                    loop {
                        tokio::time::sleep(Duration::from_secs(
                            constants::mqtt_topic::HEARTBEAT_INTERVAL_SECS,
                        ))
                        .await;

                        let devices = db_hb.load_all_devices().await;
                        let hw_serials: Vec<String> = devices
                            .iter()
                            .filter(|d| d.state == constants::device_state::DEVICE)
                            .map(|d| d.hw_serial.clone())
                            .collect();

                        let tasks = eng.get_tasks().await;
                        let executing: Vec<String> = tasks
                            .iter()
                            .filter(|t| t.status == constants::task_status::EXECUTING)
                            .map(|t| t.id.clone())
                            .collect();

                        let timeout_secs = constants::debug::HEARTBEAT_PUBLISH_TIMEOUT_SECS;
                        match tokio::time::timeout(
                            Duration::from_secs(timeout_secs),
                            mqtt_hb.publish_heartbeat(hw_serials, executing),
                        )
                        .await
                        {
                            Ok(Err(e)) => {
                                if !e.contains("未连接") {
                                    eprintln!("[heartbeat] 发送失败: {}", e);
                                }
                            },
                            Err(_) => {
                                eprintln!("[heartbeat] 发送超时 ({}s)", timeout_secs);
                            },
                            _ => {},
                        }
                    }
                });
            }

            app.manage(AppState { db, mqtt, engine, http });

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
            sync_tasks_by_phones,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
