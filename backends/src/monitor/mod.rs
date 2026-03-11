use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tauri::Emitter;

use crate::connection;
use crate::constants;
use crate::storage::{self, DeviceRow};

/// 全局线程计数器（限制并发属性获取线程数）
pub(crate) static PROP_FETCH_THREADS: AtomicUsize = AtomicUsize::new(0);

/// P1 修复：RAII guard，确保计数器在 panic/正常退出时都能回收
struct PropFetchGuard;
impl Drop for PropFetchGuard {
    fn drop(&mut self) {
        PROP_FETCH_THREADS.fetch_sub(1, Ordering::SeqCst);
    }
}

// ─── 辅助函数 ──────────────────────────────────────────────────

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

/// FIX #14: 使用 block_in_place 避免阻塞 tokio worker 线程
fn db_block_on<F, T>(rt: &tokio::runtime::Handle, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::task::block_in_place(|| rt.block_on(f))
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

// ─── 启动后台设备监控 ──────────────────────────────────────────

pub fn spawn_device_monitor(
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
                        // P0 优化：使用 spawn_blocking 复用 Tokio 阻塞线程池
                        rt_cb.spawn(async move {
                            // P1 修复：RAII guard 确保 panic 时也回收计数器
                            let _guard = PropFetchGuard;
                            let result = tokio::task::spawn_blocking(move || {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    fetch_device_row(&serial, state)
                                }))
                            })
                            .await;
                            match result {
                                Ok(Ok(row)) => {
                                    db_inner.upsert_device(&row).await;
                                    let _ = handle_inner
                                        .emit(constants::tauri_event::DEVICES_CHANGED, ());
                                },
                                Ok(Err(_)) => {
                                    eprintln!("[monitor] fetch_device_row panic");
                                },
                                Err(e) => {
                                    eprintln!("[monitor] spawn_blocking join 失败: {}", e);
                                },
                            }
                            // guard 在此 drop，自动 fetch_sub
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

    // ── 线程 2: 电池/温度定时刷新（R4 优化：改用 Tokio 异步任务）──
    let db_battery = Arc::clone(&db);
    let handle_battery = handle.clone();
    let rt_battery = rt;
    rt_battery.spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(
                constants::timing::BATTERY_REFRESH_INTERVAL_SECS,
            ))
            .await;

            let devices = db_battery.load_all_devices().await;
            let online_serials: Vec<String> = devices
                .iter()
                .filter(|dev| dev.state == constants::device_state::DEVICE)
                .map(|d| d.serial.clone())
                .collect();

            if online_serials.is_empty() {
                continue;
            }

            // 并行刷新电池（spawn_blocking 复用 Tokio 阻塞线程池）
            let mut changed = false;
            let mut handles = Vec::new();
            for serial in online_serials.iter().take(constants::limits::MAX_BATTERY_REFRESH_THREADS)
            {
                let serial = serial.clone();
                handles.push(tokio::task::spawn_blocking(move || {
                    let raw = connection::run_adb_timed(
                        connection::adb_command().args(["-s", &serial, "shell", "dumpsys battery"]),
                        constants::timing::ADB_COMMAND_TIMEOUT_SECS,
                    )
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();

                    let battery_level = parse_battery_field(&raw, "level").unwrap_or(-1);
                    let battery_temp_raw = parse_battery_field(&raw, "temperature").unwrap_or(-1);
                    if battery_level >= 0 && battery_temp_raw >= 0 {
                        Some((serial, battery_level, battery_temp_raw as f64 / 10.0))
                    } else {
                        None
                    }
                }));
            }
            for h in handles {
                if let Ok(Some((serial, level, temp))) = h.await {
                    db_battery.update_device_props(&serial, level, temp).await;
                    changed = true;
                }
            }

            if changed {
                let _ = handle_battery.emit(constants::tauri_event::DEVICES_CHANGED, ());
            }

            // WiFi 设备并行重连（spawn_blocking）
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
                let mut wifi_handles = Vec::new();
                for addr in wifi_devices {
                    wifi_handles.push(tokio::task::spawn_blocking(move || {
                        let output = connection::run_adb_timed(
                            connection::adb_command().args(["connect", &addr]),
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
                    }));
                }
                for h in wifi_handles {
                    let _ = h.await;
                }
            }
        }
    });
}
