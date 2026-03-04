use crate::connection::{DeviceManager, ShellResult};
use crate::storage::DeviceRow;
use crate::{connection, constants, AppState};
use tauri::Emitter;

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
pub async fn remove_device(
    serial: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    DeviceManager::disconnect_wifi(&serial);
    state.db.delete_device(&serial).await;
    Ok(format!("设备 {} 已移除", serial))
}

#[tauri::command]
pub async fn list_devices(state: tauri::State<'_, AppState>) -> Result<Vec<DeviceRow>, String> {
    Ok(state.db.load_all_devices().await)
}

#[tauri::command]
pub async fn execute_shell(serial: String, command: String) -> Result<ShellResult, String> {
    // FIX #3: 限制危险命令（在 release 模式下拒绝可能破坏设备的操作）
    #[cfg(not(debug_assertions))]
    {
        let cmd_lower = command.to_lowercase();
        let denied_patterns = [
            "rm -rf",
            "mkfs",
            "dd if=",
            "reboot",
            "shutdown",
            "factory_reset",
            "wipe",
            "format",
            "su -c",
        ];
        for pattern in &denied_patterns {
            if cmd_lower.contains(pattern) {
                return Err(format!("危险命令被拒绝: {}", pattern));
            }
        }
    }
    tokio::task::spawn_blocking(move || DeviceManager::new().execute_shell(&serial, &command))
        .await
        .map_err(|e| format!("执行失败: {}", e))
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
pub async fn install_apk(serial: String, apk_path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || DeviceManager::new().install_apk(&serial, &apk_path))
        .await
        .map_err(|e| format!("执行失败: {}", e))?
}

#[tauri::command]
pub async fn reboot_device(serial: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || DeviceManager::new().reboot_device(&serial))
        .await
        .map_err(|e| format!("执行失败: {}", e))?
}

#[tauri::command]
pub async fn push_file(
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
pub async fn pull_file(
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
