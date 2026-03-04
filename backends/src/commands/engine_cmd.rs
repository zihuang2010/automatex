use crate::task_provider::Task;
use crate::{constants, AppState};

#[tauri::command]
pub async fn engine_get_tasks(state: tauri::State<'_, AppState>) -> Result<Vec<Task>, String> {
    Ok(state.engine()?.get_tasks().await)
}

#[tauri::command]
pub async fn engine_start_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.start_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
pub async fn engine_pause_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.pause_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
pub async fn engine_resume_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.resume_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
pub async fn engine_stop_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.stop_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
pub async fn engine_retry_task(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.engine()?.retry_task(&task_id).await?;
    Ok(constants::response::OK.into())
}

#[tauri::command]
pub async fn engine_get_ready_serials(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<String>, String> {
    Ok(state.engine()?.get_ready_serials().await)
}

#[tauri::command]
pub async fn engine_release_offline(
    online_serials: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<u32, String> {
    Ok(state.engine()?.release_offline_devices(&online_serials).await)
}

#[tauri::command]
pub async fn engine_reorder_cities(
    task_id: String,
    new_order: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.engine()?.reorder_cities(&task_id, new_order).await
}
