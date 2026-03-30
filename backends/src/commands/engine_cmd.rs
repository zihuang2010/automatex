use crate::task_provider::Task;
use crate::{constants, AppState};

fn build_task_progress_payload(task: &Task) -> serde_json::Value {
    let total_kw: i32 = task.cities.iter().map(|c| c.total).sum();
    let done_kw: i32 = task.cities.iter().map(|c| c.done).sum();
    let progress = if total_kw > 0 {
        ((done_kw as f64 / total_kw as f64) * 100.0).round() as i32
    } else {
        0
    };

    let active_city = task.cities.iter().find(|c| c.status == "active");

    serde_json::json!({
        "task_id": task.id,
        "status": task.status,
        "progress": progress,
        "total_keywords": total_kw,
        "done_keywords": done_kw,
        "active_city": active_city.map(|c| serde_json::json!({
            "name": c.name,
            "progress": c.progress,
            "done": c.done,
            "total": c.total,
        })),
        "device": task.assigned_device,
    })
}

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

/// 异步任务进度流：前端订阅特定任务的实时进度
///
/// 使用 watch channel 驱动的增量推送，前端无需轮询。
#[tauri::command]
pub async fn subscribe_task_progress(
    task_id: String,
    on_progress: tauri::ipc::Channel<serde_json::Value>,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let engine = state.engine()?;
    let mut rx = engine.subscribe_tasks();

    let send_snapshot = |tasks: &[Task]| -> Result<bool, String> {
        let Some(task) = tasks.iter().find(|t| t.id == task_id) else {
            let _ = on_progress.send(serde_json::json!({
                "status": "not_found",
                "message": "任务不存在"
            }));
            return Ok(false);
        };

        if on_progress.send(build_task_progress_payload(task)).is_err() {
            return Err("前端进度通道已关闭".to_string());
        }

        Ok(task.status == "executing" || task.status == "paused")
    };

    if !send_snapshot(rx.borrow().as_slice())? {
        return Ok(());
    }

    while rx.changed().await.is_ok() {
        if !send_snapshot(rx.borrow().as_slice())? {
            break;
        }
    }

    Ok(())
}
