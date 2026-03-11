//! 启动阶段辅助函数
//!
//! 从 `lib.rs` 提取的 4 个启动辅助函数：
//! - `ensure_client_id` — 确保 DB 中存在 mqtt_client_id
//! - `ensure_mqtt_defaults` — 首次启动写入 MQTT 默认配置
//! - `check_daily_reset` — 跨日检测与重置
//! - `startup_sync_tasks` — 验证已绑定手机号并同步任务

use std::sync::Arc;
use tauri::Emitter;

use crate::{constants, engine::TaskEngine, http, storage, utils};

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

/// 启动时自动同步：验证已绑定手机号 → 清理冲突 → 拉取最新任务
pub(crate) async fn startup_sync_tasks(
    db: &Arc<storage::Database>,
    http: &Arc<dyn http::ApiClient>,
    engine: &Arc<TaskEngine>,
    client_id: &str,
    app_handle: &tauri::AppHandle,
) {
    use constants::{setting_key, tauri_event};

    let synced_phones: Vec<String> =
        serde_json::from_str(&db.get_setting(setting_key::SYNCED_PHONES).await.unwrap_or_default())
            .unwrap_or_default();

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
        phones: synced_phones.clone(),
        force: false,
    };

    match http.bind_phones(&bind_req).await {
        Ok(bind_resp) => {
            if !bind_resp.conflicts.is_empty() {
                let conflict_phones: Vec<String> =
                    bind_resp.conflicts.iter().map(|c| c.phone.clone()).collect();
                eprintln!("[startup] 检测到异地登录冲突: {:?}, 清理关联任务", conflict_phones);
                engine.handle_phones_unbind(conflict_phones).await;
            }

            let valid_phones = bind_resp.bound;

            if valid_phones.is_empty() {
                eprintln!("[startup] 所有手机号已失效，通知前端跳转绑定页面");
                db.set_setting(setting_key::SYNCED_PHONES, "[]").await;
                let _ = app_handle.emit(
                    tauri_event::ACCOUNT_SYNC_CHANGED,
                    serde_json::json!({ "phones": Vec::<String>::new() }),
                );
                let _ = app_handle.emit(
                    tauri_event::REQUIRE_PHONE_BIND,
                    serde_json::json!({
                        "reason": "all_expired",
                        "message": "已绑定的手机号已在其他设备登录，请重新绑定"
                    }),
                );
            } else {
                eprintln!("[startup] 有效手机号: {:?}, 拉取最新任务...", valid_phones);

                match http.fetch_tasks_by_phones(client_id, &valid_phones).await {
                    Ok(resp) => {
                        let mut server_task_ids: Vec<String> = Vec::new();
                        let mut upsert_items: Vec<(String, String, String, i64, String)> =
                            Vec::new();
                        for (phone, defs) in &resp.phone_tasks {
                            for def in defs {
                                let payload =
                                    serde_json::to_string(&def.cities).unwrap_or_default();
                                upsert_items.push((
                                    def.id.clone(),
                                    def.name.clone(),
                                    payload,
                                    1,
                                    phone.clone(),
                                ));
                                server_task_ids.push(def.id.clone());
                            }
                        }
                        db.batch_upsert_task_defs(upsert_items).await;

                        let server_task_ids: std::collections::HashSet<String> =
                            server_task_ids.into_iter().collect();
                        let local_defs = db.load_all_task_defs().await;
                        let stale_ids: Vec<String> = local_defs
                            .iter()
                            .filter(|(id, _, _, _)| !server_task_ids.contains(id))
                            .map(|(id, _, _, _)| id.clone())
                            .collect();
                        if !stale_ids.is_empty() {
                            eprintln!(
                                "[startup] 清理 {} 个本地过期任务: {:?}",
                                stale_ids.len(),
                                stale_ids
                            );
                            db.batch_cleanup_tasks(&stale_ids).await;
                        }

                        db.set_setting(
                            setting_key::SYNCED_PHONES,
                            &serde_json::to_string(&valid_phones).unwrap_or_default(),
                        )
                        .await;
                        let _ = app_handle.emit(
                            tauri_event::ACCOUNT_SYNC_CHANGED,
                            serde_json::json!({ "phones": valid_phones }),
                        );
                        engine.reload_tasks().await;
                        eprintln!(
                            "[startup] 同步完成: {} 个手机号, {} 个任务",
                            valid_phones.len(),
                            server_task_ids.len()
                        );
                        let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "done");
                    },
                    Err(e) => {
                        eprintln!("[startup] 拉取任务失败: {}, 使用本地缓存", e);
                        let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "error");
                    },
                }
            }
        },
        Err(e) => {
            eprintln!("[startup] 验证绑定失败(网络?): {}, 使用本地缓存", e);
            let _ = app_handle.emit(tauri_event::STARTUP_SYNC_STATUS, "error");
        },
    }
}
