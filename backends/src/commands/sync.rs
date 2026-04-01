use crate::{constants, http, task_sync, utils, AppState};
use tauri::Emitter;

fn parse_synced_phones(raw: String) -> Vec<String> {
    serde_json::from_str(&raw).unwrap_or_default()
}

async fn resolve_client_id(state: &AppState) -> String {
    let settings = state.db.get_all_settings().await;
    settings
        .get(constants::setting_key::MQTT_CLIENT_ID)
        .cloned()
        .unwrap_or_else(utils::generate_machine_client_id)
}

#[tauri::command]
pub async fn sync_tasks_by_phones(
    phones: Vec<String>,
    force: bool,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let engine = state.engine()?;
    let client_id = resolve_client_id(&state).await;
    let old_phones = parse_synced_phones(
        state
            .db
            .get_setting(constants::setting_key::SYNCED_PHONES)
            .await
            .unwrap_or_default(),
    );

    if phones.is_empty() {
        if !old_phones.is_empty() {
            eprintln!("[sync] 收到空手机号同步请求，执行本地清理: {:?}", old_phones);
            engine.handle_phones_unbind(old_phones).await;
        }

        state.db.set_setting(constants::setting_key::SYNCED_PHONES, "[]").await;
        let _ = app_handle.emit(
            constants::tauri_event::ACCOUNT_SYNC_CHANGED,
            serde_json::json!({ "phones": [] }),
        );

        engine.reload_tasks().await;
        engine.force_emit_update().await;

        return Ok(serde_json::json!({
            "status": constants::response::OK,
            "phones": 0,
            "tasks": 0,
        }));
    }

    let http = state.http()?;
    let bind_req =
        http::PhoneBindRequest { client_id: client_id.clone(), phones: phones.clone(), force };
    let bind_resp = http.bind_phones(&bind_req).await?;

    if !bind_resp.conflicts.is_empty() && !force {
        return Ok(serde_json::json!({
            "status": "conflicts",
            "conflicts": bind_resp.conflicts,
            "taskItems": bind_resp.task_items,
        }));
    }

    let removed_phones: Vec<String> =
        old_phones.into_iter().filter(|phone| !phones.contains(phone)).collect();
    if !removed_phones.is_empty() {
        eprintln!("[sync] 检测到被移除的手机号: {:?}，清理旧任务数据", removed_phones);
        engine.handle_phones_unbind(removed_phones).await;
    }

    let task_ids = bind_resp.task_items.clone().unwrap_or_default();
    let count = task_sync::load_remote_tasks_by_ids(http, &state.db, &task_ids).await?;

    state
        .db
        .set_setting(
            constants::setting_key::SYNCED_PHONES,
            &serde_json::to_string(&phones).unwrap_or_default(),
        )
        .await;

    let _ = app_handle.emit(
        constants::tauri_event::ACCOUNT_SYNC_CHANGED,
        serde_json::json!({ "phones": phones }),
    );

    engine.reload_tasks().await;
    engine.force_emit_update().await;

    eprintln!("[sync] 同步完成: {} 个手机号, {} 个任务", bind_req.phones.len(), count);

    Ok(serde_json::json!({
        "status": constants::response::OK,
        "phones": bind_req.phones.len(),
        "tasks": count,
    }))
}

#[tauri::command]
pub async fn unbind_phone(
    phone: String,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let engine = state.engine()?;
    let client_id = resolve_client_id(&state).await;
    let old_phones = parse_synced_phones(
        state
            .db
            .get_setting(constants::setting_key::SYNCED_PHONES)
            .await
            .unwrap_or_default(),
    );

    if !old_phones.iter().any(|saved| saved == &phone) {
        return Ok(serde_json::json!({
            "status": constants::response::OK,
            "phones": old_phones.len(),
            "tasks": state.db.load_all_task_defs().await.len(),
        }));
    }

    let http = state.http()?;
    http.unbind_phones(&http::UnbindPhonesRequest { client_id, phones: vec![phone.clone()] })
        .await?;

    engine.handle_phones_unbind(vec![phone.clone()]).await;

    let remaining_phones: Vec<String> =
        old_phones.into_iter().filter(|saved| saved != &phone).collect();
    state
        .db
        .set_setting(
            constants::setting_key::SYNCED_PHONES,
            &serde_json::to_string(&remaining_phones).unwrap_or_default(),
        )
        .await;

    let _ = app_handle.emit(
        constants::tauri_event::ACCOUNT_SYNC_CHANGED,
        serde_json::json!({ "phones": remaining_phones }),
    );

    engine.reload_tasks().await;
    engine.force_emit_update().await;

    let task_count = state.db.load_all_task_defs().await.len();
    Ok(serde_json::json!({
        "status": constants::response::OK,
        "phones": parse_synced_phones(
            state
                .db
                .get_setting(constants::setting_key::SYNCED_PHONES)
                .await
                .unwrap_or_default()
        ).len(),
        "tasks": task_count,
    }))
}
