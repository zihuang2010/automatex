use crate::{constants, http, task_sync, utils, AppState};
use std::collections::HashSet;
use tauri::Emitter;
use tracing::{debug, info};

fn parse_synced_phones(raw: String) -> Vec<String> {
    let phones: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
    task_sync::normalize_unique_strings(phones)
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
    let phones = task_sync::normalize_unique_strings(phones);
    let old_phones = parse_synced_phones(
        state
            .db
            .get_setting(constants::setting_key::SYNCED_PHONES)
            .await
            .unwrap_or_default(),
    );

    if phones.is_empty() {
        if !old_phones.is_empty() {
            info!(old_phones = ?old_phones, "收到空手机号同步请求，执行本地清理");
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
    let bind_req = http::PhoneBindRequest {
        client_id: client_id.clone(),
        mobiles: phones.clone(),
        force_bind: force,
    };
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
        info!(removed_phones = ?removed_phones, "检测到被移除的手机号，清理旧任务数据");
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

    info!(phone_count = bind_req.mobiles.len(), task_count = count, "同步完成");

    Ok(serde_json::json!({
        "status": constants::response::OK,
        "phones": bind_req.mobiles.len(),
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
    let phone = phone.trim().to_string();
    if phone.is_empty() {
        return Err("手机号不能为空".to_string());
    }

    if !old_phones.iter().any(|saved| saved == &phone) {
        return Ok(serde_json::json!({
            "status": constants::response::OK,
            "phones": old_phones.len(),
            "tasks": state.db.load_all_task_defs().await.len(),
        }));
    }

    let http = state.http()?;
    http.unbind_phones(&http::UnbindPhonesRequest { client_id, mobiles: vec![phone.clone()] })
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

/// 确认 MQTT 下发的「账号异地绑定」通知
///
/// 与 [`unbind_phone`] 的区别：
/// - 不调用 HTTP `/unbind`，因为服务端已经把这些号码解绑（MQTT 通知就是事后广播）
/// - 支持批量
/// - 用于前端用户在弹窗里点击「我知道了」后的本地清理入口
///
/// 行为：
/// 1. 计算 `remaining_phones = synced_phones - normalized`
/// 2. 收集要清理的 task_id：
///    a) 先按 `get_tasks_by_phone` 匹配（仅对 `a_task_defs.phone` 列精确相等的任务生效）
///    b) **安全网**：如果 `remaining_phones` 清空，那么本地所有任务都应该随之清空 —
///       追加 `load_all_task_defs()` 返回的全部 task_id，兜住因历史 `phone` 列为空
///       或格式不一致导致上一步漏掉的任务（修复「0 账号 + N 个孤儿任务」）
/// 3. 调用 `engine.cleanup_task_ids` 原子性地停 runtime + 删 DB + 移 s.tasks
/// 4. 更新 `synced_phones` 设置为 `remaining_phones`
/// 5. 广播 `ACCOUNT_SYNC_CHANGED` + reload + force emit
/// 6. 返回剩余手机号数量与剩余任务数量，供前端决定是否跳转到过渡页
#[tauri::command]
pub async fn acknowledge_phones_unbind(
    phones: Vec<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let normalized = task_sync::normalize_unique_strings(phones);

    let old_phones = parse_synced_phones(
        state
            .db
            .get_setting(constants::setting_key::SYNCED_PHONES)
            .await
            .unwrap_or_default(),
    );

    if normalized.is_empty() {
        debug!("ack: normalized 为空，直接返回当前状态");
        return Ok(serde_json::json!({
            "status": constants::response::OK,
            "phones": old_phones.len(),
            "tasks": state.db.load_all_task_defs().await.len(),
        }));
    }

    // 计算剩余手机号（移除与被解绑集合的交集）
    let to_remove: HashSet<&String> = normalized.iter().collect();
    let remaining_phones: Vec<String> =
        old_phones.iter().filter(|saved| !to_remove.contains(*saved)).cloned().collect();

    // 快路径：服务端推送的手机号与本地 synced_phones 完全不相交 → 本地无需任何清理。
    // 无论 remaining == old 还是 old 本就不包含被解绑号码，这里都不应触碰引擎 / DB，
    // 直接返回当前状态，避免引擎 round-trip 卡住前端「处理中...」按钮。
    if remaining_phones.len() == old_phones.len() {
        debug!(
            pushed = ?normalized,
            synced = ?old_phones,
            "ack: 服务端推送的手机号不在本地 synced_phones 中，快路径返回"
        );
        let task_count = state.db.load_all_task_defs().await.len();
        return Ok(serde_json::json!({
            "status": constants::response::OK,
            "phones": old_phones.len(),
            "tasks": task_count,
        }));
    }

    info!(unbind_phones = ?normalized, "确认手机号解绑通知，开始本地清理");

    // 收集要清理的 task_id（a） —— 精确按手机号匹配
    let mut task_ids_to_cleanup: HashSet<String> = HashSet::new();
    for phone in &normalized {
        for id in state.db.get_tasks_by_phone(phone).await {
            task_ids_to_cleanup.insert(id);
        }
    }

    // 收集要清理的 task_id（b）—— 安全网：
    // 如果确认后 `remaining_phones` 为空，说明本地应该没有任何手机号绑定、
    // 也就不应再保留任何任务；把所有残留 task_id 都加进来，兜住 phone 列
    // 历史空值导致的漏清理
    if remaining_phones.is_empty() {
        let all_defs = state.db.load_all_task_defs().await;
        let before = task_ids_to_cleanup.len();
        for (task_id, _, _, _) in all_defs {
            task_ids_to_cleanup.insert(task_id);
        }
        let extra = task_ids_to_cleanup.len() - before;
        if extra > 0 {
            info!(extra, "remaining_phones 为空，安全网追加兜底清理孤儿任务（phone 列不匹配）");
        }
    }

    // 只有在真的有 task_id 需要清理时才走引擎 round-trip
    let cleanup_ids: Vec<String> = task_ids_to_cleanup.into_iter().collect();
    let cleaned = if cleanup_ids.is_empty() {
        debug!("ack: 无匹配任务，跳过引擎清理 round-trip");
        0
    } else {
        let engine = state.engine()?;
        engine.cleanup_task_ids(cleanup_ids).await
    };
    info!(cleaned, "异地绑定通知：本地任务清理完成");

    // 更新 synced_phones 设置
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

    // 只有真的删了任务才需要 reload（否则引擎内存中的 s.tasks 已经是最新）
    if cleaned > 0 {
        let engine = state.engine()?;
        engine.reload_tasks().await;
    }

    let task_count = state.db.load_all_task_defs().await.len();
    info!(
        remaining_phones = remaining_phones.len(),
        remaining_tasks = task_count,
        cleaned,
        "解绑通知处理完成"
    );

    Ok(serde_json::json!({
        "status": constants::response::OK,
        "phones": remaining_phones.len(),
        "tasks": task_count,
    }))
}
