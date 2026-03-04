use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tauri::Emitter;

use crate::connection;
use crate::constants;
use crate::storage::{self, DeviceRow};

/// 全局线程计数器（限制并发属性获取线程数）
pub(crate) static PROP_FETCH_THREADS: AtomicUsize = AtomicUsize::new(0);

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

        // WiFi 设备并行重连
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
