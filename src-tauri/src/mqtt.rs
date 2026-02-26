use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::Mutex;

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
            client_id: format!("automatex-{}", std::process::id()),
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

/// MQTT 客户端管理器
pub struct MqttManager {
    client: Arc<Mutex<Option<AsyncClient>>>,
    status: Arc<Mutex<MqttStatus>>,
    config: Arc<Mutex<Option<MqttConfig>>>,
}

impl MqttManager {
    pub fn new() -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(MqttStatus::Disconnected)),
            config: Arc::new(Mutex::new(None)),
        }
    }

    /// 连接 MQTT Broker
    pub async fn connect(
        &self,
        config: MqttConfig,
        app_handle: tauri::AppHandle,
    ) -> Result<String, String> {
        // 如果已连接，先断开
        self.disconnect().await.ok();

        *self.status.lock().await = MqttStatus::Connecting;

        let mut opts = MqttOptions::new(&config.client_id, &config.broker_host, config.broker_port);
        opts.set_keep_alive(Duration::from_secs(30));

        if let (Some(ref user), Some(ref pass)) = (&config.username, &config.password) {
            if !user.is_empty() {
                opts.set_credentials(user, pass);
            }
        }

        let (client, mut eventloop) = AsyncClient::new(opts, 100);

        *self.client.lock().await = Some(client);
        *self.config.lock().await = Some(config);

        let status = self.status.clone();

        // 启动事件循环
        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(event) => {
                        match &event {
                            Event::Incoming(Incoming::ConnAck(_)) => {
                                *status.lock().await = MqttStatus::Connected;
                                // 通知前端连接成功
                                let _ = app_handle.emit("mqtt-status", "connected");
                            }
                            Event::Incoming(Incoming::Publish(publish)) => {
                                let topic = publish.topic.clone();
                                let payload = String::from_utf8_lossy(&publish.payload).to_string();
                                // 推送消息到前端
                                let _ = app_handle.emit(
                                    "mqtt-message",
                                    serde_json::json!({
                                        "topic": topic,
                                        "payload": payload,
                                    }),
                                );
                            }
                            Event::Incoming(Incoming::Disconnect) => {
                                *status.lock().await = MqttStatus::Disconnected;
                                let _ = app_handle.emit("mqtt-status", "disconnected");
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("{}", e);
                        *status.lock().await = MqttStatus::Error(err_msg.clone());
                        let _ = app_handle.emit("mqtt-status", format!("error:{}", err_msg));
                        // 连接失败时退出循环
                        break;
                    }
                }
            }
        });

        Ok("MQTT 连接中...".to_string())
    }

    /// 断开连接
    pub async fn disconnect(&self) -> Result<String, String> {
        if let Some(client) = self.client.lock().await.take() {
            client
                .disconnect()
                .await
                .map_err(|e| format!("断开失败: {}", e))?;
        }
        *self.status.lock().await = MqttStatus::Disconnected;
        Ok("MQTT 已断开".to_string())
    }

    /// 订阅主题
    pub async fn subscribe(&self, topic: &str) -> Result<String, String> {
        let guard = self.client.lock().await;
        let client = guard.as_ref().ok_or("MQTT 未连接".to_string())?;
        client
            .subscribe(topic, QoS::AtLeastOnce)
            .await
            .map_err(|e| format!("订阅失败: {}", e))?;
        Ok(format!("已订阅: {}", topic))
    }

    /// 发布消息
    pub async fn publish(&self, topic: &str, payload: &str) -> Result<String, String> {
        let guard = self.client.lock().await;
        let client = guard.as_ref().ok_or("MQTT 未连接".to_string())?;
        client
            .publish(topic, QoS::AtLeastOnce, false, payload.as_bytes())
            .await
            .map_err(|e| format!("发布失败: {}", e))?;
        Ok(format!("已发布到: {}", topic))
    }

    /// 获取连接状态
    pub async fn get_status(&self) -> MqttStatus {
        self.status.lock().await.clone()
    }
}
