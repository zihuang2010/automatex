//! 消息路由 — 根据 Topic 后缀将 MQTT 消息映射到 Tauri 前端事件

use crate::constants::{mqtt_topic, tauri_event};
use tauri::Emitter;

fn validate_string_array_field(json_value: &serde_json::Value, field: &str) -> Option<Vec<String>> {
    let values = json_value.get(field)?.as_array()?;
    let collected: Vec<String> = values
        .iter()
        .filter_map(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if collected.is_empty() {
        None
    } else {
        Some(collected)
    }
}

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
    let json_value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[mqtt] 消息 JSON 解析失败: topic={}, err={}", topic, e);
            return;
        },
    };

    // 过滤旧消息：如果消息携带 ts 字段且早于本次连接时间，跳过
    if let Some(ts) = json_value.get("ts").and_then(|v| v.as_i64()) {
        if ts < connect_ts {
            eprintln!(
                "[mqtt] 过滤旧消息: topic={}, msg_ts={}, connect_ts={}",
                topic, ts, connect_ts
            );
            return;
        }
    }

    // 基础消息来源校验 — 防止回环（source == 自身 client_id 时跳过）
    if let Some(source) = json_value.get("source").and_then(|v| v.as_str()) {
        if source == client_id {
            eprintln!("[mqtt] 跳过回环消息: topic={}, source={}", topic, source);
            return;
        }
    }

    if topic.ends_with(mqtt_topic::DOWN_DEVICE_KICK) {
        if validate_string_array_field(&json_value, "hw_serials").is_none() {
            eprintln!("[mqtt] 丢弃非法踢设备消息: topic={}, payload={}", topic, payload);
            return;
        }
        eprintln!("[mqtt] ⚠ 收到踢设备指令: {}", payload);
        let _ = app_handle.emit(tauri_event::MQTT_DEVICE_KICK, json_value);
    } else if topic.ends_with(mqtt_topic::DOWN_TASK_RELOAD) {
        let action = json_value.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid_action = matches!(action, "reload_all" | "reload_task" | "delete_task");
        let has_task_id =
            json_value.get("task_id").and_then(|v| v.as_str()).map(|s| !s.trim().is_empty());
        if !valid_action
            || ((action == "reload_task" || action == "delete_task")
                != has_task_id.unwrap_or(false))
        {
            eprintln!("[mqtt] 丢弃非法任务变更消息: topic={}, payload={}", topic, payload);
            return;
        }
        eprintln!("[mqtt] 收到任务变更通知: {}", payload);
        let _ = app_handle.emit(tauri_event::MQTT_TASK_RELOAD, json_value);
    } else if topic.ends_with(mqtt_topic::DOWN_PHONES_UNBIND) {
        if validate_string_array_field(&json_value, "phones").is_none() {
            eprintln!("[mqtt] 丢弃非法手机号解绑消息: topic={}, payload={}", topic, payload);
            return;
        }
        eprintln!("[mqtt] ⚠ 收到手机号解绑通知: {}", payload);
        let _ = app_handle.emit(tauri_event::MQTT_PHONES_UNBIND, json_value);
    } else if topic.ends_with(mqtt_topic::BROADCAST_TASK_UPDATE) {
        let action = json_value.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(action, "reload_all" | "reload_task" | "delete_task") {
            eprintln!("[mqtt] 丢弃非法全局任务广播: topic={}, payload={}", topic, payload);
            return;
        }
        eprintln!("[mqtt] 收到全局任务广播: {}", payload);
        let _ = app_handle.emit(tauri_event::MQTT_TASK_RELOAD, json_value);
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
