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

// ─── 共用尾巴：清任务 + 持久化 + 广播 ─────────────────────────────
//
// `sync_tasks_by_phones` / `unbind_phone` / `acknowledge_phones_unbind`
// 三个命令的共同后半段：
//   1. 调 engine 按 phone 清理任务（惟一的 destructive cleanup 入口）
//   2. 把 `final_synced_phones` 持久化到 SYNCED_PHONES 设置
//   3. 广播 ACCOUNT_SYNC_CHANGED 事件给前端
//
// 不负责：
//   - HTTP /bind 或 /unbind（调用方按业务语义决定是否调）
//   - engine.reload_tasks（时机不一样：sync 必须在 load_remote 之后才 reload，
//     而 unbind/ack 可以紧跟清理，因此 reload 由调用方自行触发）
//
// `phones_to_cleanup` 与 `final_synced_phones` 是解耦的两个概念：
//   - sync 场景：cleanup = old - new，final = new（新旧差集 + 完整新列表）
//   - unbind/ack 场景：cleanup = removed，final = old - removed
async fn apply_phone_state_change(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    phones_to_cleanup: &[String],
    final_synced_phones: &[String],
) -> u32 {
    let cleaned = if phones_to_cleanup.is_empty() {
        0
    } else {
        match state.engine() {
            Ok(engine) => engine.handle_phones_unbind(phones_to_cleanup.to_vec()).await,
            Err(_) => 0,
        }
    };

    state
        .db
        .set_setting(
            constants::setting_key::SYNCED_PHONES,
            &serde_json::to_string(final_synced_phones).unwrap_or_default(),
        )
        .await;

    let _ = app_handle.emit(
        constants::tauri_event::ACCOUNT_SYNC_CHANGED,
        serde_json::json!({ "phones": final_synced_phones }),
    );

    cleaned
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

    // 空输入：相当于「全部解绑」，走共用尾巴
    if phones.is_empty() {
        if !old_phones.is_empty() {
            info!(old_phones = ?old_phones, "收到空手机号同步请求，执行本地清理");
        }
        let cleaned = apply_phone_state_change(&state, &app_handle, &old_phones, &[]).await;
        if cleaned > 0 {
            engine.reload_tasks().await;
        }
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

    // 清理 removed_phones + 持久化完整新列表 + 广播
    let removed_phones: Vec<String> =
        old_phones.into_iter().filter(|phone| !phones.contains(phone)).collect();
    if !removed_phones.is_empty() {
        info!(removed_phones = ?removed_phones, "检测到被移除的手机号，清理旧任务数据");
    }
    let _ = apply_phone_state_change(&state, &app_handle, &removed_phones, &phones).await;

    // 加载新任务 → 必须在 set_setting 之后、reload_tasks 之前
    let task_ids = bind_resp.task_items.clone().unwrap_or_default();
    let count = task_sync::load_remote_tasks_by_ids(http, &state.db, &task_ids).await?;

    // sync 场景：无论是否有 removed，都要 reload（因为加载了新任务）
    engine.reload_tasks().await;

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

    // 本地不存在该号 → 直接返回当前状态，不触碰 HTTP/引擎/DB
    if !old_phones.iter().any(|saved| saved == &phone) {
        return Ok(serde_json::json!({
            "status": constants::response::OK,
            "phones": old_phones.len(),
            "tasks": state.db.load_all_task_defs().await.len(),
        }));
    }

    // 先调服务端 /unbind —— 失败直接返回，本地状态保持不动
    let http = state.http()?;
    http.unbind_phones(&http::UnbindPhonesRequest { client_id, mobiles: vec![phone.clone()] })
        .await?;

    let remaining_phones: Vec<String> =
        old_phones.into_iter().filter(|saved| saved != &phone).collect();
    let cleaned =
        apply_phone_state_change(&state, &app_handle, &[phone], &remaining_phones).await;
    if cleaned > 0 {
        engine.reload_tasks().await;
    }

    let task_count = state.db.load_all_task_defs().await.len();
    Ok(serde_json::json!({
        "status": constants::response::OK,
        "phones": remaining_phones.len(),
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
/// 1. normalize，空输入 → 快路径返回
/// 2. 计算 remaining_phones = old_phones - normalized
///    - 若完全不相交（remaining == old）→ 快路径返回，避免无意义的引擎 round-trip
/// 3. 调 `apply_phone_state_change(intersected, remaining)` 做清理 + 持久化 + 广播
/// 4. 仅在真的清了任务时才触发 reload_tasks
/// 5. 返回剩余手机号数量与剩余任务数量，供前端决定是否跳转过渡页
///
/// 注意：旧实现里的「remaining_phones 为空时安全网补全孤儿任务」逻辑已经删除。
/// 现在靠 migration v16 一次性清掉 phone='' 的孤儿行 + upsert 运行时 error! 告警
/// 双重保障，`get_tasks_by_phone` 成为唯一可信的定位路径。
#[tauri::command]
pub async fn acknowledge_phones_unbind(
    phones: Vec<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let engine = state.engine()?;
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

    // 计算剩余手机号（old - normalized）与实际要清理的交集
    let to_remove: HashSet<&String> = normalized.iter().collect();
    let remaining_phones: Vec<String> =
        old_phones.iter().filter(|saved| !to_remove.contains(*saved)).cloned().collect();
    let intersected: Vec<String> = old_phones
        .iter()
        .filter(|saved| to_remove.contains(*saved))
        .cloned()
        .collect();

    // 快路径：服务端推送的手机号与本地完全不相交 → 本地无需任何动作
    if intersected.is_empty() {
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

    info!(unbind_phones = ?intersected, "确认手机号解绑通知，开始本地清理");

    let cleaned =
        apply_phone_state_change(&state, &app_handle, &intersected, &remaining_phones).await;
    if cleaned > 0 {
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
