//! 消息路由 — 根据 Topic 后缀将 MQTT 消息映射到 Tauri 前端事件

use crate::constants::{mqtt_topic, tauri_event};
use tauri::Emitter;

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
        eprintln!("[mqtt] ⚠ 收到踢设备指令: {}", payload);
        let _ = app_handle.emit(tauri_event::MQTT_DEVICE_KICK, json_value);
    } else if topic.ends_with(mqtt_topic::DOWN_TASK_RELOAD) {
        eprintln!("[mqtt] 收到任务变更通知: {}", payload);
        let _ = app_handle.emit(tauri_event::MQTT_TASK_RELOAD, json_value);
    } else if topic.ends_with(mqtt_topic::DOWN_PHONES_UNBIND) {
        eprintln!("[mqtt] ⚠ 收到手机号解绑通知: {}", payload);
        let _ = app_handle.emit(tauri_event::MQTT_PHONES_UNBIND, json_value);
    } else if topic.ends_with(mqtt_topic::BROADCAST_TASK_UPDATE) {
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
