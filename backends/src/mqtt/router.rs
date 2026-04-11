//! 消息路由 — 根据 Topic 后缀将 MQTT 消息映射到 Tauri 前端事件

use crate::constants::{mqtt_topic, tauri_event};
use tauri::Emitter;
use tracing::{debug, info, warn};

use super::types::{MsgBroadcastOffline, MsgTaskChanged, MsgUnbind};

/// 根据 Topic 后缀将消息路由到不同的 Tauri 前端事件
/// `client_id`: 当前客户端 ID，用于回环校验
/// `connect_ts`: 本次连接的时间戳，早于此时间戳的消息将被过滤
pub(crate) fn route_message(
    topic: &str,
    payload: &str,
    app_handle: &tauri::AppHandle,
    client_id: &str,
    connect_ts: i64,
) {
    // 先做通用 JSON 解析，用于 ts/source 过滤
    let json_value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            warn!(topic = topic, error = %e, "消息 JSON 解析失败");
            return;
        },
    };

    // 过滤旧消息：如果消息携带 ts 字段且早于本次连接时间，跳过
    if let Some(ts) = json_value.get("ts").and_then(|v| v.as_i64()) {
        if ts < connect_ts {
            debug!(topic = topic, msg_ts = ts, connect_ts = connect_ts, "过滤旧消息");
            return;
        }
    }

    // 基础消息来源校验 — 防止回环（source == 自身 client_id 时跳过）
    if let Some(source) = json_value.get("source").and_then(|v| v.as_str()) {
        if source == client_id {
            return;
        }
    }

    // ── 下行路由（按 docs/MQTT.md 文档定义，使用类型化反序列化）──

    if topic.ends_with(mqtt_topic::DOWN_TASK_RELOAD) {
        // §2.4.1 taskChanged
        let msg: MsgTaskChanged = match serde_json::from_str(payload) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, payload = payload, "taskChanged 反序列化失败");
                return;
            },
        };
        if !matches!(msg.action.as_str(), "reload_all" | "reload_task" | "delete_task") {
            warn!(topic = topic, action = %msg.action, "丢弃非法 action");
            return;
        }
        if (msg.action == "reload_task" || msg.action == "delete_task")
            && msg.task_id.trim().is_empty()
        {
            warn!(topic = topic, "reload/delete 缺少 taskId");
            return;
        }
        info!(msg = ?msg, "收到任务变更通知");
        let _ = app_handle.emit(tauri_event::MQTT_TASK_RELOAD, msg);
    } else if topic.ends_with(mqtt_topic::DOWN_PHONES_UNBIND) {
        // §2.4.2 unbind
        let msg: MsgUnbind = match serde_json::from_str(payload) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, payload = payload, "unbind 反序列化失败");
                return;
            },
        };
        if msg.mobiles.is_empty() {
            warn!(topic = topic, "丢弃空 mobiles 解绑消息");
            return;
        }
        warn!(msg = ?msg, "收到手机号解绑通知");
        let _ = app_handle.emit(tauri_event::MQTT_PHONES_UNBIND, msg);
    } else if topic.ends_with(mqtt_topic::BROADCAST_OFFLINE) {
        // §2.4.3 broadcast/offline
        let msg: MsgBroadcastOffline = match serde_json::from_str(payload) {
            Ok(m) => m,
            Err(_) => MsgBroadcastOffline { event_at: None },
        };
        warn!(msg = ?msg, "收到广播下线指令");
        let _ = app_handle.emit(tauri_event::MQTT_BROADCAST_OFFLINE, msg);
    } else {
        let _ = app_handle.emit(
            tauri_event::MQTT_MESSAGE,
            serde_json::json!({
                "topic": topic,
                "payload": payload,
            }),
        );
    }
}
