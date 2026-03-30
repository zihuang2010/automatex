use crate::{constants, http, utils, AppState};
use tauri::Emitter;

#[tauri::command]
pub async fn sync_tasks_by_phones(
    phones: Vec<String>,
    force: bool,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let engine = state.engine()?;

    let s = state.db.get_all_settings().await;
    let client_id = s
        .get(constants::setting_key::MQTT_CLIENT_ID)
        .cloned()
        .unwrap_or_else(|| utils::generate_machine_client_id());

    let old_phones: Vec<String> = serde_json::from_str(
        &state
            .db
            .get_setting(constants::setting_key::SYNCED_PHONES)
            .await
            .unwrap_or_default(),
    )
    .unwrap_or_default();

    if phones.is_empty() {
        if !old_phones.is_empty() {
            eprintln!("[sync] 收到空手机号同步请求，执行本地清理: {:?}", old_phones);
            engine.handle_phones_unbind(old_phones).await;
        }

        state.db.set_setting(constants::setting_key::SYNCED_PHONES, "[]").await;
        let _ =
            app_handle.emit(constants::tauri_event::ACCOUNT_SYNC_CHANGED, serde_json::json!({ "phones": [] }));

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
            "bound": bind_resp.bound,
            "conflicts": bind_resp.conflicts,
        }));
    }

    let bound_phones = if force { phones.clone() } else { bind_resp.bound };

    // 清理被移除的手机号
    let removed_phones: Vec<String> =
        old_phones.into_iter().filter(|p| !bound_phones.contains(p)).collect();
    if !removed_phones.is_empty() {
        eprintln!("[sync] 检测到被移除的手机号: {:?}，清理旧任务数据", removed_phones);
        engine.handle_phones_unbind(removed_phones).await;
    }

    let resp = http.fetch_tasks_by_phones(&client_id, &bound_phones).await?;

    // FIX #11: 收集所有任务定义，使用批量 upsert
    let mut upsert_items: Vec<(String, String, String, i64, String)> = Vec::new();
    let mut count = 0usize;
    for (phone, defs) in &resp.phone_tasks {
        for def in defs {
            let payload = serde_json::to_string(&def.cities).unwrap_or_default();
            upsert_items.push((def.id.clone(), def.name.clone(), payload, 1, phone.clone()));
            count += 1;
        }
    }
    state.db.batch_upsert_task_defs(upsert_items).await;

    // 清理本地残留的过期任务定义（服务端已不返回的 task_id）
    let server_ids: std::collections::HashSet<String> =
        resp.phone_tasks.values().flatten().map(|d| d.id.clone()).collect();
    let local_defs = state.db.load_all_task_defs().await;
    let stale_ids: Vec<String> = local_defs
        .iter()
        .filter(|(id, _, _, _)| !server_ids.contains(id))
        .map(|(id, _, _, _)| id.clone())
        .collect();
    if !stale_ids.is_empty() {
        eprintln!("[sync] 清理 {} 个本地过期任务: {:?}", stale_ids.len(), stale_ids);
        state.db.batch_cleanup_tasks(&stale_ids).await;
    }

    state
        .db
        .set_setting(
            constants::setting_key::SYNCED_PHONES,
            &serde_json::to_string(&bound_phones).unwrap_or_default(),
        )
        .await;

    // 推送账号变更事件到前端
    let _ = app_handle.emit(
        constants::tauri_event::ACCOUNT_SYNC_CHANGED,
        serde_json::json!({ "phones": bound_phones }),
    );

    engine.reload_tasks().await;
    engine.force_emit_update().await;

    eprintln!("[sync] 同步完成: {} 个手机号, {} 个任务", bound_phones.len(), count);

    Ok(serde_json::json!({
        "status": constants::response::OK,
        "phones": bound_phones.len(),
        "tasks": count,
    }))
}
