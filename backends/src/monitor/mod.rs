use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// MEM-3 修复：RAII guard，确保 inflight_props 中的 serial 在任何退出路径都被移除。
/// 单纯依赖手动 remove 时，若 catch_unwind 未捕获该 panic，
/// 或未来代码删掉 catch_unwind 后， serial 将永远残留导致属性永不刷新。
struct InFlightGuard {
    serial: String,
    inflight: Arc<Mutex<HashSet<String>>>,
}
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut set = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
        set.remove(&self.serial);
    }
}

#[derive(Debug, Clone, Copy)]
struct WifiReconnectState {
    failure_count: u32,
    next_allowed_at: Instant,
}

/// Windows 兼容：不使用双引号，避免 CreateProcess 二次转义导致 adb shell 收到畸形命令
const BATCH_PROPS_CMD: &str = "echo __MODEL__=$(getprop ro.product.model) && \
    echo __BRAND__=$(getprop ro.product.brand) && \
    echo __ANDROID__=$(getprop ro.build.version.release) && \
    echo __SDK__=$(getprop ro.build.version.sdk) && \
    echo __SERIAL__=$(getprop ro.serialno) && \
    echo __BOOTSERIAL__=$(getprop ro.boot.serialno) && \
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

fn placeholder_device_row(serial: &str, state: &str, existing: Option<&DeviceRow>) -> DeviceRow {
    let device_type = if serial.contains(':') {
        constants::device_type::WIFI
    } else {
        constants::device_type::USB
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    DeviceRow {
        serial: serial.to_string(),
        hw_serial: existing.map(|row| row.hw_serial.clone()).unwrap_or_else(|| serial.to_string()),
        name: existing
            .map(|row| row.name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| serial.to_string()),
        device_type: existing
            .map(|row| row.device_type.clone())
            .unwrap_or_else(|| device_type.to_string()),
        address: existing
            .and_then(|row| row.address.clone())
            .or_else(|| (device_type == constants::device_type::WIFI).then(|| serial.to_string())),
        state: state.to_string(),
        model: existing
            .map(|row| row.model.clone())
            .unwrap_or_else(|| constants::device_state::UNKNOWN.to_string()),
        brand: existing
            .map(|row| row.brand.clone())
            .unwrap_or_else(|| constants::device_state::UNKNOWN.to_string()),
        android_version: existing
            .map(|row| row.android_version.clone())
            .unwrap_or_else(|| constants::device_state::UNKNOWN.to_string()),
        sdk_version: existing
            .map(|row| row.sdk_version.clone())
            .unwrap_or_else(|| constants::device_state::UNKNOWN.to_string()),
        display_resolution: existing
            .map(|row| row.display_resolution.clone())
            .unwrap_or_else(|| constants::device_state::UNKNOWN.to_string()),
        battery_level: existing.map(|row| row.battery_level).unwrap_or(-1),
        battery_temperature: existing.map(|row| row.battery_temperature).unwrap_or(-1.0),
        is_flagged: existing.map(|row| row.is_flagged).unwrap_or(false),
        updated_at: now,
    }
}

fn device_props_incomplete(row: &DeviceRow) -> bool {
    let unknown = constants::device_state::UNKNOWN;
    [
        row.model.as_str(),
        row.brand.as_str(),
        row.android_version.as_str(),
        row.sdk_version.as_str(),
        row.display_resolution.as_str(),
        row.hw_serial.as_str(),
        row.name.as_str(),
    ]
    .iter()
    .any(|value| value.trim().is_empty() || *value == unknown)
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

    let raw = match connection::run_adb_timed(
        connection::adb_command().args(["-s", serial, "shell", BATCH_PROPS_CMD]),
        constants::timing::ADB_COMMAND_TIMEOUT_SECS,
    ) {
        Ok(output) => {
            if output.status.success() {
                String::from_utf8_lossy(&output.stdout).to_string()
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!(
                    "[monitor] adb shell 命令失败 (serial={}): exit={}, stderr={}",
                    serial,
                    output.status,
                    stderr.trim()
                );
                String::new()
            }
        },
        Err(e) => {
            eprintln!(
                "[monitor] ⚠ adb 进程启动失败 (serial={}): {} — 请检查 adb 二进制是否完整 (Windows 需要 AdbWinApi.dll)",
                serial, e
            );
            String::new()
        },
    };

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

fn schedule_devices_changed_emit(
    handle: &tauri::AppHandle,
    rt: &tokio::runtime::Handle,
    emit_pending: &Arc<AtomicBool>,
) {
    if emit_pending.swap(true, Ordering::SeqCst) {
        return;
    }

    let handle = handle.clone();
    let emit_pending = Arc::clone(emit_pending);
    rt.spawn(async move {
        tokio::time::sleep(Duration::from_millis(constants::timing::DEVICE_EVENT_DEBOUNCE_MS))
            .await;
        let _ = handle.emit(constants::tauri_event::DEVICES_CHANGED, ());
        emit_pending.store(false, Ordering::SeqCst);
    });
}

fn acquire_prop_fetch_slot() -> bool {
    PROP_FETCH_THREADS
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            if current < constants::limits::MAX_PROP_FETCH_THREADS {
                Some(current + 1)
            } else {
                None
            }
        })
        .is_ok()
}

fn next_reconnect_delay(failure_count: u32) -> Duration {
    let base = constants::timing::WIFI_RECONNECT_BASE_DELAY_SECS;
    let max = constants::timing::WIFI_RECONNECT_MAX_DELAY_SECS;
    let shift = failure_count.saturating_sub(1).min(8);
    let secs = base.saturating_mul(1u64 << shift).min(max);
    Duration::from_secs(secs)
}

fn rotating_batch(items: &[String], start: usize, max_batch: usize) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }

    let batch_len = items.len().min(max_batch);
    let start = start % items.len();
    (0..batch_len)
        .map(|offset| items[(start + offset) % items.len()].clone())
        .collect()
}

pub fn spawn_device_monitor(
    handle: tauri::AppHandle,
    db: Arc<storage::Database>,
    device_ready: Arc<tokio::sync::Notify>,
    rt: tokio::runtime::Handle,
) {
    let emit_pending = Arc::new(AtomicBool::new(false));
    let inflight_props = Arc::new(Mutex::new(HashSet::<String>::new()));

    let db_track = Arc::clone(&db);
    let handle_track = handle.clone();
    let rt_track = rt.clone();
    let ready_track = Arc::clone(&device_ready);
    let emit_pending_track = Arc::clone(&emit_pending);
    let inflight_props_track = Arc::clone(&inflight_props);

    std::thread::spawn(move || loop {
        let addr = std::net::SocketAddrV4::new(
            std::net::Ipv4Addr::new(127, 0, 0, 1),
            connection::adb::adb_port(),
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut server = adb_client::server::ADBServer::new(addr);
            let db_cb = Arc::clone(&db_track);
            let handle_cb = handle_track.clone();
            let ready = Arc::clone(&ready_track);
            let rt_cb = rt_track.clone();
            let emit_pending = Arc::clone(&emit_pending_track);
            let inflight_props = Arc::clone(&inflight_props_track);

            server.track_devices(move |device| {
                let serial = device.identifier.clone();
                let state = device_state_str(&device.state);
                let existing = db_block_on(&rt_cb, db_cb.get_device_by_serial(&serial));

                if state != constants::device_state::DEVICE {
                    let row = placeholder_device_row(&serial, state, existing.as_ref());
                    db_block_on(&rt_cb, db_cb.upsert_device(&row));
                    schedule_devices_changed_emit(&handle_cb, &rt_cb, &emit_pending);
                    return Ok(());
                }

                ready.notify_one();

                let needs_props = existing.as_ref().map(device_props_incomplete).unwrap_or(true);

                if !needs_props {
                    db_block_on(&rt_cb, db_cb.update_device_state(&serial, state));
                    schedule_devices_changed_emit(&handle_cb, &rt_cb, &emit_pending);
                    return Ok(());
                }

                let should_fetch = {
                    let mut inflight = inflight_props.lock().unwrap_or_else(|e| e.into_inner());
                    if inflight.contains(&serial) {
                        false
                    } else if acquire_prop_fetch_slot() {
                        inflight.insert(serial.clone());
                        true
                    } else {
                        false
                    }
                };

                if should_fetch {
                    let db_inner = Arc::clone(&db_cb);
                    let handle_inner = handle_cb.clone();
                    let rt_inner = rt_cb.clone();
                    let emit_pending_inner = Arc::clone(&emit_pending);
                    let inflight_props_inner = Arc::clone(&inflight_props);
                    let existing_for_fetch = existing.clone();
                    rt_cb.spawn(async move {
                        let _guard = PropFetchGuard;
                        // MEM-3 修复：RAII guard 替代手动 remove
                        let _inflight_guard = InFlightGuard {
                            serial: serial.clone(),
                            inflight: Arc::clone(&inflight_props_inner),
                        };
                        let serial_for_fetch = serial.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                fetch_device_row(&serial_for_fetch, state)
                            }))
                        })
                        .await;

                        match result {
                            Ok(Ok(row)) => {
                                db_inner.upsert_device(&row).await;
                                schedule_devices_changed_emit(
                                    &handle_inner,
                                    &rt_inner,
                                    &emit_pending_inner,
                                );
                            },
                            Ok(Err(_)) => {
                                eprintln!("[monitor] fetch_device_row panic");
                                let row = placeholder_device_row(
                                    &serial,
                                    state,
                                    existing_for_fetch.as_ref(),
                                );
                                db_inner.upsert_device(&row).await;
                                schedule_devices_changed_emit(
                                    &handle_inner,
                                    &rt_inner,
                                    &emit_pending_inner,
                                );
                            },
                            Err(e) => {
                                eprintln!("[monitor] spawn_blocking join 失败: {}", e);
                                let row = placeholder_device_row(
                                    &serial,
                                    state,
                                    existing_for_fetch.as_ref(),
                                );
                                db_inner.upsert_device(&row).await;
                                schedule_devices_changed_emit(
                                    &handle_inner,
                                    &rt_inner,
                                    &emit_pending_inner,
                                );
                            },
                        }
                        // _inflight_guard drop 在此自动执行 (MEM-3)
                    });
                } else {
                    let row = placeholder_device_row(&serial, state, existing.as_ref());
                    db_block_on(&rt_cb, db_cb.upsert_device(&row));
                    schedule_devices_changed_emit(&handle_cb, &rt_cb, &emit_pending);
                }

                Ok(())
            })
        }));

        if result.is_err() {
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
        std::thread::sleep(Duration::from_secs(constants::timing::ADB_RECONNECT_WAIT_SECS));
    });

    let db_reconcile = Arc::clone(&db);
    let ready_reconcile = Arc::clone(&device_ready);
    let handle_reconcile = handle.clone();
    let rt_reconcile = rt.clone();
    let emit_pending_reconcile = Arc::clone(&emit_pending);
    rt.spawn(async move {
        let mut last_online = Vec::<String>::new();
        loop {
            tokio::time::sleep(Duration::from_millis(constants::timing::DEVICE_CACHE_TTL_MS)).await;

            let port = connection::adb::adb_port();
            let result = tokio::task::spawn_blocking(move || {
                let addr = std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(127, 0, 0, 1), port);
                let mut server = adb_client::server::ADBServer::new(addr);
                server.devices().map(|devices| {
                    devices
                        .into_iter()
                        .filter(|device| {
                            matches!(device.state, adb_client::server::DeviceState::Device)
                        })
                        .map(|device| device.identifier)
                        .collect::<Vec<String>>()
                })
            })
            .await;

            match result {
                Ok(Ok(online)) => {
                    let mut online_sorted = online;
                    online_sorted.sort();
                    if online_sorted == last_online {
                        continue;
                    }
                    last_online = online_sorted.clone();
                    db_reconcile.mark_offline_except(online_sorted.clone()).await;
                    if !online_sorted.is_empty() {
                        ready_reconcile.notify_one();
                    }
                    schedule_devices_changed_emit(
                        &handle_reconcile,
                        &rt_reconcile,
                        &emit_pending_reconcile,
                    );
                },
                Ok(Err(e)) => {
                    eprintln!("[monitor] devices() 查询失败: {}", e);
                },
                Err(e) => {
                    eprintln!("[monitor] devices() 阻塞任务失败: {}", e);
                },
            }
        }
    });

    let db_battery = Arc::clone(&db);
    let handle_battery = handle.clone();
    let rt_battery = rt.clone();
    let emit_pending_battery = Arc::clone(&emit_pending);
    rt.spawn(async move {
        let mut battery_cursor = 0usize;
        let mut wifi_reconnect = HashMap::<String, WifiReconnectState>::new();

        loop {
            tokio::time::sleep(Duration::from_secs(
                constants::timing::BATTERY_REFRESH_INTERVAL_SECS,
            ))
            .await;

            let devices = db_battery.load_all_devices().await;
            let current_battery: HashMap<String, (i32, f64)> = devices
                .iter()
                .map(|device| {
                    (device.serial.clone(), (device.battery_level, device.battery_temperature))
                })
                .collect();

            let online_serials: Vec<String> = devices
                .iter()
                .filter(|device| device.state == constants::device_state::DEVICE)
                .map(|device| device.serial.clone())
                .collect();

            if !online_serials.is_empty() {
                let batch = rotating_batch(
                    &online_serials,
                    battery_cursor,
                    constants::limits::MAX_BATTERY_REFRESH_THREADS,
                );
                battery_cursor = (battery_cursor + batch.len()) % online_serials.len();

                let mut handles = Vec::new();
                for serial in batch {
                    // MEM-1 修复：改用 adb_shell_async，零阻塞事件驱动，
                    // 彻底消除 spawn_blocking + run_adb_timed 的 20ms 诞询干扰线程池。
                    handles.push(tokio::spawn(async move {
                        let raw = connection::adb::adb_shell_async(&serial, "dumpsys battery")
                            .await
                            .unwrap_or_default();
                        let battery_level = parse_battery_field(&raw, "level").unwrap_or(-1);
                        let battery_temp_raw =
                            parse_battery_field(&raw, "temperature").unwrap_or(-1);
                        if battery_level >= 0 && battery_temp_raw >= 0 {
                            Some((serial, battery_level, battery_temp_raw as f64 / 10.0))
                        } else {
                            None
                        }
                    }));
                }

                let mut changed = false;
                for handle in handles {
                    if let Ok(Some((serial, level, temp))) = handle.await {
                        let prev = current_battery.get(&serial).copied();
                        if prev != Some((level, temp)) {
                            db_battery.update_device_props(&serial, level, temp).await;
                            changed = true;
                        }
                    }
                }

                if changed {
                    schedule_devices_changed_emit(
                        &handle_battery,
                        &rt_battery,
                        &emit_pending_battery,
                    );
                }
            } else {
                battery_cursor = 0;
            }

            let offline_wifi: Vec<String> = devices
                .iter()
                .filter(|device| {
                    device.device_type == constants::device_type::WIFI
                        && device.state == constants::device_state::OFFLINE
                        && device.serial.contains(':')
                })
                .map(|device| device.serial.clone())
                .collect();
            let offline_wifi_set: HashSet<String> = offline_wifi.iter().cloned().collect();
            wifi_reconnect.retain(|serial, _| offline_wifi_set.contains(serial));

            let now = Instant::now();
            let mut eligible: Vec<(String, Instant)> = offline_wifi
                .into_iter()
                .map(|serial| {
                    let next_allowed_at = wifi_reconnect
                        .get(&serial)
                        .map(|state| state.next_allowed_at)
                        .unwrap_or(now);
                    (serial, next_allowed_at)
                })
                .filter(|(_, next_allowed_at)| *next_allowed_at <= now)
                .collect();
            eligible.sort_by_key(|(_, next_allowed_at)| *next_allowed_at);

            let reconnect_targets: Vec<String> = eligible
                .into_iter()
                .take(constants::limits::MAX_WIFI_RECONNECT_PER_CYCLE)
                .map(|(serial, _)| serial)
                .collect();

            let mut reconnect_handles = Vec::new();
            for serial in reconnect_targets {
                reconnect_handles.push(tokio::spawn(async move {
                    let result = connection::adb::connect_wifi_via_adb_async(&serial).await;
                    (serial, result)
                }));
            }

            for handle in reconnect_handles {
                if let Ok((serial, result)) = handle.await {
                    match result {
                        Ok(_) => {
                            wifi_reconnect.remove(&serial);
                            eprintln!("[wifi-reconnect] 重连成功: {}", serial);
                        },
                        Err(err) => {
                            let next_state = {
                                let entry = wifi_reconnect.entry(serial.clone()).or_insert(
                                    WifiReconnectState { failure_count: 0, next_allowed_at: now },
                                );
                                entry.failure_count = entry.failure_count.saturating_add(1);
                                entry.next_allowed_at =
                                    Instant::now() + next_reconnect_delay(entry.failure_count);
                                *entry
                            };
                            eprintln!(
                                "[wifi-reconnect] 重连失败: {} ({}), 下次尝试于 {:?}",
                                serial, err, next_state.next_allowed_at
                            );
                        },
                    }
                }
            }
        }
    });
}
