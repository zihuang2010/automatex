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
mod task_sync;
pub mod utils;

use commands::*;
use engine::TaskEngine;
use mqtt::MqttManager;
use startup::{check_daily_reset, ensure_client_id, ensure_mqtt_defaults, startup_sync_tasks};
use std::sync::Arc;
use std::time::Duration;
use tauri::{Emitter, Listener, Manager};
use tracing::{debug, error, info, warn};

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
    // ── 日志初始化（必须最先执行）──
    let default_level = if cfg!(debug_assertions) { "debug" } else { "info" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!(
            "{default_level},automatex_lib={default_level},hyper=warn,reqwest=warn,rumqttc=info"
        ))
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .with_writer(std::io::stderr)
        .init();

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
                let mqtt_init = Arc::clone(&mqtt);
                let app_handle = app.handle().clone();
                let device_ready_clone = Arc::clone(&device_ready);
                tauri::async_runtime::spawn(async move {
                    let rt = tokio::runtime::Handle::current();
                    let _ = app_handle
                        .emit(constants::tauri_event::STARTUP_SYNC_STATUS, "booting:prepare");

                    // 打包资源准备与完整性校验（启动时执行一次）
                    connection::adb::prepare_packaged_sidecars(&app_handle);
                    connection::adb::verify_sidecar_integrity(Some(&app_handle));

                    // ADB 端口探测：如果 5037 被占用，自动尝试 5038-5047
                    connection::adb::resolve_adb_port();

                    check_daily_reset(&db_init).await;
                    let _ = app_handle
                        .emit(constants::tauri_event::STARTUP_SYNC_STATUS, "booting:database");

                    tokio::join!(
                        db_init.cleanup_orphan_runs(),
                        db_init.mark_offline_except(Vec::new()),
                        db_init.cleanup_stale_assignments(),
                    );

                    let client_id = ensure_client_id(&db_init).await;
                    ensure_mqtt_defaults(&db_init).await;
                    let startup_settings = db_init.get_all_settings().await;
                    let _ = app_handle
                        .emit(constants::tauri_event::STARTUP_SYNC_STATUS, "booting:http-client");

                    let http_base_url = startup_settings
                        .get(constants::setting_key::API_BASE_URL)
                        .cloned()
                        .unwrap_or_default();
                    if http_base_url.is_empty() {
                        warn!("api_base_url 未配置，HTTP API 调用将失败");
                    }
                    let http_client: Arc<dyn http::ApiClient> =
                        Arc::new(http::RealApiClient::new(&http_base_url));
                    let _ = http_cell.set(Arc::clone(&http_client));

                    // 设备监控尽早启动，与引擎构建和任务同步并行。
                    monitor::spawn_device_monitor(
                        app_handle.clone(),
                        Arc::clone(&db_init),
                        Arc::clone(&device_ready_clone),
                        rt.clone(),
                    );
                    let _ = app_handle
                        .emit(constants::tauri_event::STARTUP_SYNC_STATUS, "booting:monitor");

                    let _ = app_handle
                        .emit(constants::tauri_event::STARTUP_SYNC_STATUS, "booting:engine");
                    let eng = TaskEngine::new(
                        Arc::clone(&db_init),
                        Arc::clone(&http_client),
                        Arc::clone(&mqtt_init),
                        app_handle.clone(),
                    )
                    .await;
                    let _ = engine_cell.set(Arc::clone(&eng));

                    info!("异步初始化完成，引擎已就绪");
                    let _ = app_handle
                        .emit(constants::tauri_event::STARTUP_SYNC_STATUS, "booting:engine-ready");

                    let startup_settings_for_mqtt = startup_settings.clone();
                    let app_mqtt = app_handle.clone();
                    let mqtt_connect = async move {
                        let has_host = startup_settings_for_mqtt
                            .contains_key(constants::setting_key::MQTT_HOST);
                        let auto_off = startup_settings_for_mqtt
                            .get(constants::setting_key::MQTT_AUTO_CONNECT)
                            .map(|v| v == "false")
                            .unwrap_or(false);

                        if has_host && !auto_off {
                            let config =
                                crate::commands::build_mqtt_config_from(&startup_settings_for_mqtt);
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            info!(host = %config.broker_host, port = config.broker_port, "MQTT 自动连接");
                            let mqtt_state = app_mqtt.state::<AppState>();
                            match mqtt_state.mqtt.connect(config, app_mqtt.clone()).await {
                                Ok(msg) => info!("{}", msg),
                                Err(e) => error!(error = %e, "MQTT 自动连接失败"),
                            }
                        } else {
                            info!("MQTT 未配置主机或已禁用自动连接，跳过");
                        }
                    };

                    // 任务同步优先完成，再建立 MQTT 连接，避免竞态：
                    // MQTT 连接成功后立刻收到 taskChanged 通知时任务缓存尚未就绪
                    startup_sync_tasks(&db_init, &http_client, &eng, &client_id, &app_handle).await;
                    mqtt_connect.await;
                    let _ = app_handle.emit(constants::tauri_event::STARTUP_SYNC_STATUS, "ready");
                });
            }

            // ── MQTT 事件监听 ──
            {
                let engine_kick = Arc::clone(&engine);
                app.listen(constants::tauri_event::MQTT_DEVICE_KICK, move |event| {
                    let engine = Arc::clone(&engine_kick);
                    tauri::async_runtime::spawn(async move {
                        let Some(eng) = engine.get() else {
                            warn!("engine 未初始化，跳过 device-kick");
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
                                info!(count = n, "踢设备完成");
                            }
                        }
                    });
                });

                let engine_reload = Arc::clone(&engine);
                app.listen(constants::tauri_event::MQTT_TASK_RELOAD, move |event| {
                    let engine = Arc::clone(&engine_reload);
                    tauri::async_runtime::spawn(async move {
                        let Some(eng) = engine.get() else {
                            warn!("engine 未初始化，跳过 task-reload");
                            return;
                        };
                        if let Ok(msg) =
                            serde_json::from_str::<mqtt::MsgTaskChanged>(event.payload())
                        {
                            let task_id_ref = if msg.task_id.is_empty() {
                                None
                            } else {
                                Some(msg.task_id.as_str())
                            };
                            eng.handle_task_reload(&msg.action, task_id_ref).await;
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
                            warn!("engine 未初始化，跳过 phones-unbind");
                            return;
                        };
                        if let Ok(msg) = serde_json::from_str::<mqtt::MsgUnbind>(event.payload()) {
                            let n = eng.handle_phones_unbind(msg.mobiles).await;
                            info!(removed = n, "手机号解绑完成");
                        }
                    });
                });
            }

            // ── MQTT 广播下线监听 ──
            {
                let engine_broadcast = Arc::clone(&engine);
                let mqtt_broadcast = Arc::clone(&mqtt);
                app.listen(
                    constants::tauri_event::MQTT_BROADCAST_OFFLINE,
                    move |_event| {
                        let engine = Arc::clone(&engine_broadcast);
                        let mqtt = Arc::clone(&mqtt_broadcast);
                        tauri::async_runtime::spawn(async move {
                            warn!("收到广播下线，停止所有任务并断开 MQTT");
                            if let Some(eng) = engine.get() {
                                eng.stop_all_tasks().await;
                            }
                            let _ = mqtt.disconnect().await;
                        });
                    },
                );
            }

            // ── MQTT 心跳定时器 ──
            {
                let mqtt_hb = Arc::clone(&mqtt);
                let db_hb = Arc::clone(&db);
                tauri::async_runtime::spawn(async move {
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

                        let timeout_secs = constants::debug::HEARTBEAT_PUBLISH_TIMEOUT_SECS;
                        match tokio::time::timeout(
                            Duration::from_secs(timeout_secs),
                            mqtt_hb.publish_heartbeat(hw_serials),
                        )
                        .await
                        {
                            Ok(Err(e)) => {
                                if !e.contains("未连接") {
                                    debug!(error = %e, "心跳发送失败");
                                }
                            },
                            Err(_) => {
                                warn!(timeout_secs, "心跳发送超时");
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
            get_keyword_results,
            get_task_run_stats,
            engine_get_tasks,
            engine_get_task_detail,
            engine_start_task,
            engine_pause_task,
            engine_resume_task,
            engine_stop_task,
            engine_retry_task,
            engine_get_ready_serials,
            engine_release_offline,
            engine_reorder_cities,
            engine_clear_task_device,
            subscribe_task_progress,
            flag_device,
            unflag_device,
            sync_tasks_by_phones,
            unbind_phone,
            scrcpy_start_mirror,
            scrcpy_stop_mirror,
            scrcpy_inject_touch,
            scrcpy_inject_scroll,
            scrcpy_inject_key,
            scrcpy_inject_text,
            scrcpy_press_back,
            scrcpy_reset_video,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // P0 修复：应用退出时优雅关闭所有投屏会话和引擎
                let state = app_handle.state::<AppState>();
                let scrcpy = Arc::clone(&state.scrcpy);
                let engine = Arc::clone(&state.engine);
                tauri::async_runtime::block_on(async move {
                    scrcpy.shutdown().await;
                    // 引擎 shutdown：drop sender 触发 event_loop 退出 + 取消所有 worker
                    if let Some(eng) = engine.get() {
                        eng.shutdown().await;
                    }
                    info!("资源清理完成");
                });
            }
        });
}
