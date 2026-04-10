use crate::storage::{DailyStatRow, DailySummary, ResultRow, TaskRunStats};
use crate::task_provider::{self, Task};
use crate::{constants, AppState};

#[tauri::command]
pub async fn list_tasks(state: tauri::State<'_, AppState>) -> Result<Vec<Task>, String> {
    Ok(task_provider::load_tasks(&state.db).await)
}

#[tauri::command]
pub async fn get_task_detail(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<Task, String> {
    task_provider::load_task_by_id(&state.db, &task_id)
        .await
        .ok_or_else(|| format!("任务 {} 不存在", task_id))
}

#[tauri::command]
pub async fn get_daily_stats(
    device_serial: String,
    run_date: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<DailyStatRow>, String> {
    Ok(state.db.query_daily_stats(&device_serial, &run_date).await)
}

#[tauri::command]
pub async fn get_daily_summary(
    run_date: String,
    state: tauri::State<'_, AppState>,
) -> Result<DailySummary, String> {
    Ok(state.db.query_daily_summary(&run_date).await)
}

#[tauri::command]
pub async fn get_task_run_stats(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<TaskRunStats, String> {
    Ok(state.db.query_task_run_stats(&task_id).await)
}

#[tauri::command]
pub async fn get_keyword_results(
    task_id: String,
    city_name: String,
    keyword_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ResultRow>, String> {
    Ok(state.db.get_keyword_results(&task_id, &city_name, &keyword_name).await)
}

#[tauri::command]
pub async fn clear_task_progress(
    task_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.db.clear_task_progress(&task_id).await;
    state.db.delete_task_state(&task_id).await;
    Ok(constants::response::OK.to_string())
}
