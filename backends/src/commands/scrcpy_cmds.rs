//! Scrcpy 投屏 Tauri Commands

use crate::AppState;

/// S-1 修复：校验设备序列号格式
fn validate_serial(serial: &str) -> Result<(), String> {
    if serial.is_empty() || serial.len() > 64 {
        return Err("无效的设备序列号: 长度不合法".into());
    }
    // 仅允许 字母、数字、点、冒号、连字符、下划线
    if !serial
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_'))
    {
        return Err(format!("无效的设备序列号: {}", serial));
    }
    Ok(())
}

/// 启动投屏
/// P-1 优化：通过 Channel 直接推送帧（不再 base64 + 全局事件）
#[tauri::command]
pub async fn scrcpy_start_mirror(
    serial: String,
    on_frame: tauri::ipc::Channel<crate::scrcpy::session::FramePayload>,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<crate::scrcpy::session::MirrorStartedPayload, String> {
    validate_serial(&serial)?;

    // JAR 路径：与 adb 同目录
    let jar_path = {
        let adb = crate::connection::adb::adb_path();
        let adb_dir = std::path::Path::new(adb).parent().ok_or("无法获取 adb 所在目录")?;
        let jar = adb_dir.join("scrcpy-server");
        if !jar.exists() {
            return Err(format!(
                "找不到 scrcpy-server: {} (请将 scrcpy-server 放到 adb 同目录)",
                jar.display()
            ));
        }
        jar.to_string_lossy().to_string()
    };

    state.scrcpy.start_mirror(&serial, &jar_path, app, on_frame).await
}

/// 停止投屏
#[tauri::command]
pub async fn scrcpy_stop_mirror(
    serial: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    validate_serial(&serial)?;
    state.scrcpy.stop_mirror(&serial).await
}

/// 注入触控
#[tauri::command]
pub async fn scrcpy_inject_touch(
    serial: String,
    action: u8,
    x: u32,
    y: u32,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    validate_serial(&serial)?;
    state.scrcpy.inject_touch(&serial, action, x, y).await
}

/// 注入按键
/// Q-3 修复：前端统一使用 snake_case 参数名 meta_state
#[tauri::command]
pub async fn scrcpy_inject_key(
    serial: String,
    keycode: u32,
    meta_state: u32,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    validate_serial(&serial)?;
    state.scrcpy.inject_key(&serial, keycode, meta_state).await
}

/// 返回键
#[tauri::command]
pub async fn scrcpy_press_back(
    serial: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    validate_serial(&serial)?;
    state.scrcpy.press_back(&serial).await
}
