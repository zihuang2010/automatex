mod commands;
mod connection;
pub mod constants;
mod engine;
mod http;
mod monitor;
mod mqtt;
pub(crate) mod scrcpy;
mod startup;
mod storage;
mod task_provider;
pub mod utils;

use commands::*;
use engine::TaskEngine;
use mqtt::MqttManager;
use startup::{check_daily_reset, ensure_client_id, ensure_mqtt_defaults, startup_sync_tasks};
use std::sync::Arc;
use std::time::Duration;
use tauri::{Emitter, Listener, Manager};

// ─── State ─────────────────────────────────────────────────────

pub(crate) struct AppState {
    pub db: Arc<storage::Database>,
    pub mqtt: Arc<MqttManager>,
    pub engine: Arc<tokio::sync::OnceCell<Arc<TaskEngine>>>,
    pub http: Arc<tokio::sync::OnceCell<Arc<dyn http::ApiClient>>>,
    pub scrcpy: Arc<scrcpy::session::SessionManager>,
}

impl AppState {
    pub fn engine(&self) -> Result<&Arc<TaskEngine>, String> {
        self.engine.get().ok_or_else(|| "引擎正在初始化，请稍后重试".to_string())
    }

    pub fn http(&self) -> Result<&Arc<dyn http::ApiClient>, String> {
        self.http.get().ok_or_else(|| "HTTP 客户端正在初始化，请稍后重试".to_string())
    }
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

            let mqtt = Arc::new(MqttManager::new());
            let engine: Arc<tokio::sync::OnceCell<Arc<TaskEngine>>> =
                Arc::new(tokio::sync::OnceCell::new());
            let http: Arc<tokio::sync::OnceCell<Arc<dyn http::ApiClient>>> =
                Arc::new(tokio::sync::OnceCell::new());

            let device_ready = Arc::new(tokio::sync::Notify::new());

            // ── 异步初始化 ──
            {
                let db_init = Arc::clone(&db);
                let engine_cell = Arc::clone(&engine);
                let http_cell = Arc::clone(&http);
                let app_handle = app.handle().clone();
                let device_ready_clone = Arc::clone(&device_ready);
                tauri::async_runtime::spawn(async move {
                    let rt = tokio::runtime::Handle::current();

                    check_daily_reset(&db_init).await;

                    task_provider::sync_task_cache(&db_init).await;
                    db_init.cleanup_orphan_runs().await;
                    db_init.mark_offline_except(Vec::new()).await;
                    db_init.cleanup_stale_assignments().await;

                    let client_id = ensure_client_id(&db_init).await;
                    ensure_mqtt_defaults(&db_init).await;

                    let http_base_url = db_init
                        .get_setting(constants::setting_key::API_BASE_URL)
                        .await
                        .unwrap_or_default();
                    let http_client: Arc<dyn http::ApiClient> = if http_base_url.is_empty() {
                        let mock = http::MockApiClient::new();
                        if let Some(scenario) =
                            db_init.get_setting(constants::setting_key::MOCK_SCENARIO).await
                        {
                            if !scenario.is_empty() {
                                mock.set_mock_scenario(&scenario);
                            }
                        }
                        Arc::new(mock)
                    } else {
                        Arc::new(http::RealApiClient::new(&http_base_url))
                    };
                    let _ = http_cell.set(Arc::clone(&http_client));

                    let eng = TaskEngine::new(
                        Arc::clone(&db_init),
                        Arc::clone(&http_client),
                        app_handle.clone(),
                    )
                    .await;
                    let _ = engine_cell.set(Arc::clone(&eng));

                    eprintln!("[startup] 异步初始化完成，引擎已就绪");

                    startup_sync_tasks(&db_init, &http_client, &eng, &client_id, &app_handle).await;

                    // ── MQTT 自动连接 ──
                    {
                        let db_mqtt = Arc::clone(&db_init);
                        let app_mqtt = app_handle.clone();
                        tokio::spawn(async move {
                            let startup_settings = db_mqtt.get_all_settings().await;
                            let has_host =
                                startup_settings.contains_key(constants::setting_key::MQTT_HOST);
                            let auto_off = startup_settings
                                .get(constants::setting_key::MQTT_AUTO_CONNECT)
                                .map(|v| v == "false")
                                .unwrap_or(false);

                            if has_host && !auto_off {
                                let config =
                                    crate::commands::build_mqtt_config_from(&startup_settings);
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
                    monitor::spawn_device_monitor(
                        app_handle.clone(),
                        Arc::clone(&db_init),
                        Arc::clone(&device_ready_clone),
                        rt.clone(),
                    );

                    // ── 设备归属同步 ──
                    eprintln!("[startup] 等待设备就绪...");
                    device_ready_clone.notified().await;
                    eprintln!("[startup] 设备就绪，开始归属同步");

                    let devices = db_init.load_all_devices().await;
                    let online: Vec<http::DeviceSyncItem> = devices
                        .iter()
                        .filter(|d| d.state == constants::device_state::DEVICE)
                        .map(|d| http::DeviceSyncItem {
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

                    let req = http::DeviceSyncRequest { client_id, online, offline_local };

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

            let scrcpy = Arc::new(scrcpy::session::SessionManager::new());
            app.manage(AppState { db, mqtt, engine, http, scrcpy });

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
            scrcpy_start_mirror,
            scrcpy_stop_mirror,
            scrcpy_inject_touch,
            scrcpy_inject_key,
            scrcpy_press_back,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
