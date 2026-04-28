use crate::connection::{DeviceManager, ShellResult};
use crate::storage::DeviceRow;
use crate::{connection, constants, AppState};
use tauri::Emitter;

async fn ensure_registered_device(state: &AppState, serial: &str) -> Result<(), String> {
    if state.db.device_exists(serial).await {
        Ok(())
    } else {
        Err(format!("设备 {} 未注册或已移除", serial))
    }
}

fn ensure_safe_text(value: &str, field: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{} 不能为空", field));
    }
    if trimmed.contains('\n') || trimmed.contains('\r') || trimmed.contains('\0') {
        return Err(format!("{} 包含非法控制字符", field));
    }
    Ok(())
}

fn ensure_local_path_exists(path: &str, field: &str) -> Result<(), String> {
    ensure_safe_text(path, field)?;
    if std::path::Path::new(path).exists() {
        Ok(())
    } else {
        Err(format!("{} 不存在: {}", field, path))
    }
}

fn ensure_local_parent_exists(path: &str, field: &str) -> Result<(), String> {
    ensure_safe_text(path, field)?;
    let parent = std::path::Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| format!("{} 缺少有效父目录: {}", field, path))?;
    if parent.exists() {
        Ok(())
    } else {
        Err(format!("{} 的父目录不存在: {}", field, parent.display()))
    }
}

/// SEC-3 修复：远端设备路径白名单校验，防止路径遍历攻击。
///
/// 仅允许访问安全的设备目录（/sdcard/、/data/local/tmp/ 等），
/// 拒绝包含 `..` 的路径进行巡路径遍历。
fn ensure_safe_remote_path(path: &str, field: &str) -> Result<(), String> {
    ensure_safe_text(path, field)?;

    // 重配隋防御：路径中不允许出现 .. 组件
    if path.split('/').any(|seg| seg == ".." || seg == ".") {
        return Err(format!("{} 包含非法路径组件 (.. / .): {}", field, path));
    }

    // 允许的安全目录前缀白名单
    const ALLOWED_PREFIXES: &[&str] =
        &["/sdcard/", "/storage/emulated/", "/data/local/tmp/", "/mnt/sdcard/", "/mnt/user/"];
    if !ALLOWED_PREFIXES.iter().any(|prefix| path.starts_with(prefix)) {
        return Err(format!(
            "{} 不在允许路径范围内（允许: /sdcard/ /data/local/tmp/ 等）: {}",
            field, path
        ));
    }

    Ok(())
}

#[tauri::command]
pub async fn add_device(
    address: String,
    name: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if state.db.device_exists(&address).await {
        return Err(format!("设备 {} 已存在", address));
    }
    let entry = DeviceManager::build_wifi_entry(&address, &name)?;

    connection::adb::connect_wifi_via_adb_async(&address).await?;

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
pub async fn remove_device(
    serial: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    ensure_registered_device(&state, &serial).await?;
    DeviceManager::disconnect_wifi_async(&serial).await;
    state.db.delete_device(&serial).await;
    Ok(format!("设备 {} 已移除", serial))
}

#[tauri::command]
pub async fn list_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceRow>, String> {
    Ok(state.db.load_all_devices().await)
}

/// 把当前 USB 接入的设备切换为无线 ADB：
/// `adb tcpip 5555` → 抓 wlan0 IP → 轮询 `adb connect <ip>:5555` 直到成功或超时。
/// connect 成功后立即用 USB 行的 hw_serial 主动 upsert wifi 行 + reconcile（单事务），
/// 防止 wifi transport 握手未稳时 monitor 走 placeholder 路径产生 hw_serial 占位
/// 的孤儿 wifi 行（race condition）。
#[tauri::command]
pub async fn switch_device_to_wifi(
    serial: String,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    let usb_row = state
        .db
        .get_device_by_serial(&serial)
        .await
        .ok_or_else(|| format!("设备 {} 不存在", serial))?;

    if usb_row.device_type == constants::device_type::WIFI {
        return Err("当前已是无线连接，无需切换".to_string());
    }

    let ip = connection::adb::fetch_wlan_ipv4_async(&serial).await?;
    connection::adb::enable_tcpip_async(&serial, 5555).await?;

    // adbd 重启后 ~1–2s 才能在 wifi 接口监听；不再用固定 sleep，改成轮询：
    // 首次成功立即返回，最坏 MAX_RETRIES × INTERVAL_MS 后报错给前端。
    let address = format!("{}:5555", ip);
    let mut last_err: Option<String> = None;
    let mut connected = false;
    for attempt in 0..constants::timing::WIFI_HANDSHAKE_MAX_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(
                constants::timing::WIFI_HANDSHAKE_RETRY_INTERVAL_MS,
            ))
            .await;
        }
        match connection::adb::connect_wifi_via_adb_async(&address).await {
            Ok(_) => {
                connected = true;
                break;
            },
            Err(e) => last_err = Some(e),
        }
    }
    if !connected {
        return Err(last_err.unwrap_or_else(|| "ADB WiFi 连接超时".to_string()));
    }

    // 用 USB 行属性克隆出 wifi 行（保留真实 hw_serial），原子地 upsert + reconcile，
    // 把旧 USB 行的引用迁移到 wifi serial 并删除之。命令返回时 DB 已是单行 wifi。
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut wifi_row = usb_row.clone();
    wifi_row.serial = address.clone();
    wifi_row.device_type = constants::device_type::WIFI.to_string();
    wifi_row.address = Some(address.clone());
    wifi_row.state = constants::device_state::DEVICE.to_string();
    wifi_row.updated_at = now_ts;
    state
        .db
        .upsert_and_reconcile_by_hw_serial(&wifi_row, &usb_row.hw_serial)
        .await;

    let _ = app.emit(constants::tauri_event::DEVICES_CHANGED, ());
    Ok(format!("已切换到无线 {}，可拔出 USB 数据线", address))
}

#[tauri::command]
pub async fn execute_shell(
    serial: String,
    command: String,
    state: tauri::State<'_, AppState>,
) -> Result<ShellResult, String> {
    // SEC-1 修复：编译期门控——不再依赖运行时环境变量。
    // release 构建中函数体直接返回 Err，即使运维设置环境变量也不能绕过。
    // debug 构建（开发调试）前保留完整功能。

    #[cfg(not(debug_assertions))]
    {
        // 避免未使用变量警告
        let _ = (&serial, &command, &state);
        return Err("生产模式已禁用任意 ADB shell（仅 debug 构建可用）".to_string());
    }

    #[cfg(debug_assertions)]
    {
        ensure_registered_device(&state, &serial).await?;
        ensure_safe_text(&command, "shell 命令")?;
        if command.len() > 512 {
            return Err("shell 命令过长，已拒绝执行".to_string());
        }
        match connection::adb::adb_shell_async(&serial, &command).await {
            Ok(output) => Ok(ShellResult {
                success: true,
                output: output.trim().to_string(),
                error: String::new(),
            }),
            Err(e) => Ok(ShellResult { success: false, output: String::new(), error: e }),
        }
    }
}

#[tauri::command]
pub async fn get_device_info(
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
pub async fn install_apk(
    serial: String,
    apk_path: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    ensure_registered_device(&state, &serial).await?;
    ensure_local_path_exists(&apk_path, "APK 路径")?;
    if !apk_path.to_ascii_lowercase().ends_with(".apk") {
        return Err(format!("仅允许安装 .apk 文件: {}", apk_path));
    }
    connection::adb::adb_cmd_async(&serial, &["install", &apk_path])
        .await
        .map(|_| format!("APK 安装成功: {}", apk_path))
}

#[tauri::command]
pub async fn reboot_device(
    serial: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    ensure_registered_device(&state, &serial).await?;
    connection::adb::adb_cmd_async(&serial, &["reboot"]).await?;
    Ok("设备正在重启...".to_string())
}

#[tauri::command]
pub async fn push_file(
    serial: String,
    local_path: String,
    remote_path: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    ensure_registered_device(&state, &serial).await?;
    ensure_local_path_exists(&local_path, "本地路径")?;
    // SEC-3 修复：验证远端路径安全性
    ensure_safe_remote_path(&remote_path, "远端路径")?;
    connection::adb::adb_cmd_async(&serial, &["push", &local_path, &remote_path])
        .await
        .map(|_| format!("文件已推送: {} -> {}", local_path, remote_path))
}

#[tauri::command]
pub async fn pull_file(
    serial: String,
    remote_path: String,
    local_path: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    ensure_registered_device(&state, &serial).await?;
    // SEC-3 修复：验证远端路径安全性
    ensure_safe_remote_path(&remote_path, "远端路径")?;
    ensure_local_parent_exists(&local_path, "本地保存路径")?;
    connection::adb::adb_cmd_async(&serial, &["pull", &remote_path, &local_path])
        .await
        .map(|_| format!("文件已拉取: {} -> {}", remote_path, local_path))
}

#[tauri::command]
pub async fn flag_device(
    serial: String,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    state.db.flag_device(&serial).await;
    let _ = app.emit(constants::tauri_event::DEVICES_CHANGED, ());
    Ok(format!("设备 {} 已标记风控", serial))
}

#[tauri::command]
pub async fn unflag_device(
    serial: String,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    state.db.unflag_device(&serial).await;
    let _ = app.emit(constants::tauri_event::DEVICES_CHANGED, ());
    Ok(format!("设备 {} 已解除风控标记", serial))
}
