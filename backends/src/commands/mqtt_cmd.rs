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
        .cloned()
        .unwrap_or_else(crate::utils::generate_machine_client_id);
    let username = s.get(setting_key::MQTT_USERNAME).filter(|v| !v.is_empty()).cloned();
    let password = s.get(setting_key::MQTT_PASSWORD).filter(|v| !v.is_empty()).cloned();
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
