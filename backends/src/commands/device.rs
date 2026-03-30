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
    let shell_debug_enabled = cfg!(debug_assertions)
        || std::env::var("AUTOMATEX_ALLOW_DEVICE_SHELL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

    if !shell_debug_enabled {
        return Err(
            "生产模式已禁用任意 ADB shell；如需调试，请设置 AUTOMATEX_ALLOW_DEVICE_SHELL=1"
                .to_string(),
        );
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
    connection::adb::adb_cmd_async(&serial, &["install", &apk_path])
        .await
        .map(|_| format!("APK 安装成功: {}", apk_path))
}

#[tauri::command]
pub async fn reboot_device(serial: String) -> Result<String, String> {
    connection::adb::adb_cmd_async(&serial, &["reboot"]).await?;
    Ok("设备正在重启...".to_string())
}

#[tauri::command]
pub async fn push_file(
    serial: String,
    local_path: String,
    remote_path: String,
) -> Result<String, String> {
    connection::adb::adb_cmd_async(&serial, &["push", &local_path, &remote_path])
        .await
        .map(|_| format!("文件已推送: {} -> {}", local_path, remote_path))
}

#[tauri::command]
pub async fn pull_file(
    serial: String,
    remote_path: String,
    local_path: String,
) -> Result<String, String> {
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
