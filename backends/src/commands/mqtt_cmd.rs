use crate::mqtt::{MqttConfig, MqttStatus};
use crate::utils::setting_or;
use crate::{constants, AppState};

pub(crate) fn build_mqtt_config_from(s: &std::collections::HashMap<String, String>) -> MqttConfig {
    use constants::{mqtt_default, setting_key};
    let host = setting_or(s, setting_key::MQTT_HOST, mqtt_default::host());
    let port: u16 = s
        .get(setting_key::MQTT_PORT)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(mqtt_default::port_num);
    let client_id = s
        .get(setting_key::MQTT_CLIENT_ID)
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(crate::utils::generate_machine_client_id);
    // 与 host 对称：空字符串或缺失时回退到默认凭据，避免发出无认证 CONNECT 被 broker
    // 拒绝为 NotAuthorized（历史 bug：用户在设置面板清空凭据后保存会写入空串）
    let username_str = setting_or(s, setting_key::MQTT_USERNAME, mqtt_default::username());
    let password_str = setting_or(s, setting_key::MQTT_PASSWORD, mqtt_default::password());
    let username = if username_str.is_empty() { None } else { Some(username_str) };
    let password = if password_str.is_empty() { None } else { Some(password_str) };
    MqttConfig { broker_host: host, broker_port: port, client_id, username, password }
}

#[tauri::command]
pub async fn mqtt_connect(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let s = state.db.get_all_settings().await;
    let config = build_mqtt_config_from(&s);
    state.mqtt.connect(config, app).await
}

#[tauri::command]
pub async fn mqtt_disconnect(state: tauri::State<'_, AppState>) -> Result<String, String> {
    state.mqtt.disconnect().await
}

#[tauri::command]
pub async fn mqtt_subscribe(
    topic: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.mqtt.subscribe(&topic).await
}

#[tauri::command]
pub async fn mqtt_publish(
    topic: String,
    payload: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    state.mqtt.publish(&topic, &payload).await
}

#[tauri::command]
pub async fn mqtt_status(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let status = state.mqtt.get_status().await;
    match status {
        MqttStatus::Connected => Ok(constants::mqtt_emit_status::CONNECTED.to_string()),
        MqttStatus::Connecting => Ok(constants::mqtt_emit_status::CONNECTING.to_string()),
        MqttStatus::Disconnected => Ok(constants::mqtt_emit_status::DISCONNECTED.to_string()),
        MqttStatus::Error(e) => Ok(format!("error:{}", e)),
    }
}
