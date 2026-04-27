use crate::utils::setting_or;
use crate::{constants, AppState};

#[tauri::command]
pub async fn get_settings(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let s = state.db.get_all_settings().await;

    use constants::{mqtt_default, setting_key};
    Ok(serde_json::json!({
        setting_key::MQTT_HOST: setting_or(&s, setting_key::MQTT_HOST, mqtt_default::host()),
        setting_key::MQTT_PORT: setting_or(&s, setting_key::MQTT_PORT, mqtt_default::port()),
        setting_key::MQTT_CLIENT_ID: s.get(setting_key::MQTT_CLIENT_ID).cloned()
            .unwrap_or_else(crate::utils::generate_machine_client_id),
        setting_key::MQTT_USERNAME: setting_or(&s, setting_key::MQTT_USERNAME, ""),
        setting_key::MQTT_PASSWORD: setting_or(&s, setting_key::MQTT_PASSWORD, ""),
        setting_key::API_BASE_URL: setting_or(&s, setting_key::API_BASE_URL, ""),
        setting_key::SYNCED_PHONES: setting_or(&s, setting_key::SYNCED_PHONES, "[]"),
        setting_key::THEME: setting_or(&s, setting_key::THEME, "dark"),
    }))
}

#[tauri::command]
pub async fn save_settings(
    settings: serde_json::Value,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if let Some(obj) = settings.as_object() {
        for key in obj.keys() {
            if !constants::settings::ALLOWED_KEYS.contains(&key.as_str()) {
                return Err(format!("不允许的设置项: {}", key));
            }
        }
        let pairs: Vec<(String, String)> = obj
            .iter()
            .map(|(k, v)| {
                let val = match v.as_str() {
                    Some(s) => s.to_string(),
                    None => v.to_string(),
                };
                (k.clone(), val)
            })
            .collect();
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        state.db.set_settings_batch(&refs).await;
    }
    Ok("设置已保存".to_string())
}
