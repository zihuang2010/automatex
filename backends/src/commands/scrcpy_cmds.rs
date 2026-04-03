//! Scrcpy 投屏 Tauri Commands

use crate::{
    constants,
    scrcpy::session::{ScrcpySessionStatePayload, ScrcpyTextRoutePayload},
    AppState,
};
use tauri::Emitter;

/// Android keycode 最大有效值
const MAX_ANDROID_KEYCODE: u32 = 284;

/// 校验设备序列号格式
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
#[tauri::command]
pub async fn scrcpy_start_mirror(
    serial: String,
    on_frame: tauri::ipc::Channel<tauri::ipc::Response>,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<crate::scrcpy::session::MirrorStartedPayload, String> {
    validate_serial(&serial)?;

    let jar_path = crate::connection::adb::resolve_scrcpy_server_path(&app)?
        .to_string_lossy()
        .to_string();

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
    if action > 2 {
        return Err(format!("无效触控动作: {}", action));
    }
    state.scrcpy.inject_touch(&serial, action, x, y).await
}

#[tauri::command]
pub async fn scrcpy_inject_scroll(
    serial: String,
    x: u32,
    y: u32,
    h_scroll: f32,
    v_scroll: f32,
    buttons: u32,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    validate_serial(&serial)?;
    state.scrcpy.inject_scroll(&serial, x, y, h_scroll, v_scroll, buttons).await
}

/// 注入按键
/// Tauri v2 自动将 snake_case 转为 camelCase，前端传 metaState
#[tauri::command]
pub async fn scrcpy_inject_key(
    serial: String,
    keycode: u32,
    meta_state: u32,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    validate_serial(&serial)?;
    if keycode > MAX_ANDROID_KEYCODE {
        return Err(format!("无效 keycode: {}", keycode));
    }
    state.scrcpy.inject_key(&serial, keycode, meta_state).await
}

/// 注入文本（支持中文等 UTF-8 字符，超过 300B 自动分片）
#[tauri::command]
pub async fn scrcpy_inject_text(
    serial: String,
    text: String,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    validate_serial(&serial)?;
    if text.is_empty() {
        return Ok(());
    }
    let route = state.scrcpy.inject_text(&serial, &text).await?;
    let _ = app.emit(
        constants::tauri_event::SCRCPY_TEXT_ROUTE,
        ScrcpyTextRoutePayload { serial, route: route.as_str() },
    );
    Ok(())
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

#[tauri::command]
pub async fn scrcpy_reset_video(
    serial: String,
    reason: Option<String>,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    validate_serial(&serial)?;
    let (width, height) = state.scrcpy.session_dimensions(&serial).await.unwrap_or((0, 0));
    let _ = app.emit(
        constants::tauri_event::SCRCPY_SESSION_STATE,
        ScrcpySessionStatePayload {
            serial: serial.clone(),
            phase: "recovering",
            width,
            height,
            recover_reason: reason,
            last_frame_at_ms: None,
        },
    );
    state.scrcpy.reset_video(&serial).await
}
