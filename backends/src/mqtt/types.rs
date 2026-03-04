use serde::{Deserialize, Serialize};

/// MQTT 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttConfig {
    pub broker_host: String,
    pub broker_port: u16,
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            broker_host: "127.0.0.1".to_string(),
            broker_port: 1883,
            client_id: String::new(),
            username: None,
            password: None,
        }
    }
}

/// MQTT 连接状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MqttStatus {
    Disconnected,
    Connected,
    Connecting,
    Error(String),
}
