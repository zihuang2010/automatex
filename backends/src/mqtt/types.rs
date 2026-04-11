use serde::{Deserialize, Serialize};

// ─── 连接配置 & 状态 ─────────────────────────────────────────────────────────

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

// ─── 上行消息（客户端 → 服务端）── docs/MQTT.md §2.3 ─────────────────────────

/// §2.3.1 设备上线事件
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MsgOnline<'a> {
    pub client_id: &'a str,
    pub event_at: String,
}

/// §2.3.2 设备下线事件
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MsgOffline<'a> {
    pub client_id: &'a str,
    pub event_at: String,
    pub reason: &'a str,
}

/// §2.3.3 心跳上报
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MsgHeartbeat<'a> {
    pub client_id: &'a str,
    pub event_at: String,
    pub devices: &'a [String],
}

/// §2.3.4 任务状态事件
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MsgTaskEvent<'a> {
    pub client_id: &'a str,
    pub task_id: &'a str,
    pub event_type: &'a str,
    pub event_at: String,
}

/// §2.3.5 LWT 遗嘱消息
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MsgLwt<'a> {
    pub client_id: &'a str,
    pub reason: &'a str,
}

// ─── 下行消息（服务端 → 客户端）── docs/MQTT.md §2.4 ─────────────────────────

/// §2.4.1 任务数据变更通知
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MsgTaskChanged {
    pub task_id: String,
    pub action: String,
    #[serde(default)]
    pub event_at: Option<String>,
}

/// §2.4.2 手机号解绑通知
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MsgUnbind {
    pub mobiles: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub event_at: Option<String>,
}

/// §2.4.3 截止工作时间到达（广播）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MsgBroadcastOffline {
    #[serde(default)]
    pub event_at: Option<String>,
}
