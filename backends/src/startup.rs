//! 启动阶段辅助函数
//!
//! 从 `lib.rs` 提取的 4 个启动辅助函数：
//! - `ensure_client_id` — 确保 DB 中存在 mqtt_client_id
//! - `ensure_mqtt_defaults` — 首次启动写入 MQTT 默认配置
//! - `check_daily_reset` — 跨日检测与重置
//! - `startup_sync_tasks` — 验证已绑定手机号并同步任务

use std::sync::Arc;
use std::time::Instant;
use tauri::Emitter;

use crate::{constants, engine::TaskEngine, http, storage, task_sync, utils};

/// 确保 DB 中存在 mqtt_client_id，不存在则基于机器指纹生成并持久化
pub(crate) async fn ensure_client_id(db: &storage::Database) -> String {
    use constants::setting_key;
    if let Some(existing) = db.get_setting(setting_key::MQTT_CLIENT_ID).await {
        if !existing.is_empty() {
            eprintln!("[client_id] 使用已有: {}", existing);
            return existing;
        }
    }
    let id = utils::generate_machine_client_id();
    db.set_setting(setting_key::MQTT_CLIENT_ID, &id).await;
    eprintln!("[client_id] 首次生成并持久化: {}", id);
    id
}

/// 确保 MQTT 默认配置存在（首次启动时写入）
pub(crate) async fn ensure_mqtt_defaults(db: &storage::Database) {
    use constants::{mqtt_default, setting_key};
    let host = mqtt_default::host();
    let port = mqtt_default::port();
    let username = mqtt_default::username();
    let password = mqtt_default::password();
    let defaults: &[(&str, &str)] = &[
        (setting_key::MQTT_HOST, host),
        (setting_key::MQTT_PORT, port),
        (setting_key::MQTT_USERNAME, username),
        (setting_key::MQTT_PASSWORD, password),
    ];
    let existing = db.get_all_settings().await;
    for (key, default_val) in defaults {
        let has_value = existing.get(*key).map(|v| !v.is_empty()).unwrap_or(false);
        if !has_value {
            db.set_setting(key, default_val).await;
            eprintln!(
                "[startup] MQTT 默认配置写入: {}={}",
                key,
                if *key == setting_key::MQTT_PASSWORD { "***" } else { default_val }
            );
        }
    }
}

/// 跨日检测：比较 last_active_date 与今天，不同则执行完整重置
pub(crate) async fn check_daily_reset(db: &storage::Database) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let last_date = db
        .get_setting(constants::setting_key::LAST_ACTIVE_DATE)
        .await
        .unwrap_or_default();

    if last_date == today {
        eprintln!("[startup] 日期未变 ({}), 跳过跨日重置", today);
        return;
    }

    eprintln!(
        "[startup] 检测到跨日: {} → {}, 执行重置...",
        if last_date.is_empty() { "首次" } else { &last_date },
        today
    );

    db.close_all_running_rounds().await;
    db.close_all_unfinished_runs().await;
    db.daily_reset_tasks().await;
    db.delete_all_devices().await;
    db.cleanup_synced_progress().await;
    db.set_setting(constants::setting_key::LAST_ACTIVE_DATE, &today).await;
    eprintln!("[startup] 跨日重置完成, last_active_date={}", today);
}

/// 启动时自动同步：验证已绑定手机号 → 拉取最新任务 / 处理冲突
pub(crate) async fn startup_sync_tasks(
    db: &Arc<storage::Database>,
    http: &Arc<dyn http::ApiClient>,
    engine: &Arc<TaskEngine>,
    client_id: &str,
    app_handle: &tauri::AppHandle,
) {
    use constants::{setting_key, tauri_event};
    let started_at = Instant::now();

    let raw_synced_phones = db.get_setting(setting_key::SYNCED_PHONES).await.unwrap_or_default();
    let synced_phones: Vec<String> = task_sync::normalize_unique_strings(
        serde_json::from_str::<Vec<String>>(&raw_synced_phones).unwrap_or_default(),
    );
    let normalized_synced_phones_json = serde_json::to_string(&synced_phones).unwrap_or_default();
    if normalized_synced_phones_json != raw_synced_phones {
        db.set_setting(setting_key::SYNCED_PHONES, &normalized_synced_phones_json).await;
        eprintln!("[startup] 已同步手机号已归一化去重: {:?}", synced_phones);
    }

    if synced_phones.is_empty() {
        eprintln!("[startup] 无已绑定手机号，通知前端跳转绑定页面");
        let _ = app_handle.emit(
            tauri_event::REQUIRE_PHONE_BIND,
            serde_json::json!({
                "reason": "no_phones",
                "message": "请绑定手机号后开始使用"
            }),
        );
        return;
    }

    eprintln!("[startup] 检测到已绑定手机号: {:?}, 验证有效性...", synced_phones);
    let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "syncing");

    let bind_req = http::PhoneBindRequest {
        client_id: client_id.to_string(),
        mobiles: synced_phones.clone(),
        force_bind: false,
    };

    match http.bind_phones(&bind_req).await {
        Ok(bind_resp) => {
            if !bind_resp.conflicts.is_empty() {
                let conflict_phones = task_sync::bind_conflict_phones(&bind_resp);
                eprintln!(
                    "[startup] 检测到异地登录冲突: {:?}, 清理冲突任务并保留无冲突任务",
                    conflict_phones
                );
                engine.handle_phones_unbind(conflict_phones).await;
                let count = match task_sync::load_remote_tasks_by_ids(
                    http,
                    db,
                    &bind_resp.task_items.clone().unwrap_or_default(),
                )
                .await
                {
                    Ok(count) => count,
                    Err(e) => {
                        eprintln!("[startup] 冲突态任务拉取失败: {}", e);
                        let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "error");
                        return;
                    },
                };

                engine.reload_tasks().await;
                engine.force_emit_update().await;
                let _ = app_handle.emit(
                    tauri_event::REQUIRE_PHONE_BIND,
                    serde_json::json!({
                        "reason": "conflicts",
                        "message": "部分手机号已在其他客户端绑定，请确认是否强制绑定",
                        "conflicts": bind_resp.conflicts,
                    }),
                );
                eprintln!("[startup] 冲突态任务同步完成，保留 {} 个无冲突任务", count);
                eprintln!(
                    "[startup] 启动同步结束: elapsed_ms={}",
                    started_at.elapsed().as_millis()
                );
                let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "done");
                return;
            }

            if bind_resp
                .task_items
                .as_ref()
                .map(|task_items| task_items.is_empty())
                .unwrap_or(true)
            {
                eprintln!("[startup] 所有手机号已失效，通知前端跳转绑定页面");
                let _ = app_handle.emit(
                    tauri_event::REQUIRE_PHONE_BIND,
                    serde_json::json!({
                        "reason": "all_expired",
                        "message": "已绑定的手机号已在其他设备登录，请重新绑定"
                    }),
                );
            } else {
                eprintln!("[startup] 有效手机号: {:?}, 拉取最新任务...", synced_phones);

                match task_sync::load_remote_tasks_by_ids(
                    http,
                    db,
                    &bind_resp.task_items.clone().unwrap_or_default(),
                )
                .await
                {
                    Ok(count) => {
                        let _ = app_handle.emit(
                            tauri_event::ACCOUNT_SYNC_CHANGED,
                            serde_json::json!({ "phones": synced_phones }),
                        );
                        engine.reload_tasks().await;
                        eprintln!(
                            "[startup] 同步完成: {} 个手机号, {} 个任务, elapsed_ms={}",
                            bind_req.mobiles.len(),
                            count,
                            started_at.elapsed().as_millis()
                        );
                        let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "done");
                    },
                    Err(e) => {
                        eprintln!(
                            "[startup] 拉取任务失败: {}, 使用本地缓存, elapsed_ms={}",
                            e,
                            started_at.elapsed().as_millis()
                        );
                        let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "error");
                    },
                }
            }
        },
        Err(e) => {
            eprintln!(
                "[startup] 验证绑定失败(网络?): {}, 使用本地缓存, elapsed_ms={}",
                e,
                started_at.elapsed().as_millis()
            );
            let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "error");
        },
    }
}
