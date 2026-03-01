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
/// FIX #8: 新增 loop_handle 用于等待事件循环退出
pub struct MqttManager {
    client: Arc<Mutex<Option<AsyncClient>>>,
    status: Arc<Mutex<MqttStatus>>,
    cancel_tx: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>>,
    loop_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl MqttManager {
    pub fn new() -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(MqttStatus::Disconnected)),
            cancel_tx: Arc::new(Mutex::new(None)),
            loop_handle: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn connect(
        &self,
        config: MqttConfig,
        app_handle: tauri::AppHandle,
    ) -> Result<String, String> {
        self.disconnect().await.ok();

        *self.status.lock().await = MqttStatus::Connecting;

        let mut opts = MqttOptions::new(&config.client_id, &config.broker_host, config.broker_port);
        opts.set_keep_alive(Duration::from_secs(crate::constants::timing::MQTT_KEEP_ALIVE_SECS));

        if let (Some(ref user), Some(ref pass)) = (&config.username, &config.password) {
            if !user.is_empty() {
                opts.set_credentials(user, pass);
            }
        }

        let (client, mut eventloop) = AsyncClient::new(opts, 100);
        *self.client.lock().await = Some(client);

        let status = self.status.clone();
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        *self.cancel_tx.lock().await = Some(cancel_tx);

        // FIX #8: 保存 JoinHandle，disconnect 时可以等待退出
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel_rx.changed() => {
                        if *cancel_rx.borrow() {
                            *status.lock().await = MqttStatus::Disconnected;
                            break;
                        }
                    }
                    result = eventloop.poll() => {
                        match result {
                            Ok(event) => {
                                match &event {
                                    Event::Incoming(Incoming::ConnAck(_)) => {
                                        *status.lock().await = MqttStatus::Connected;
                                        let _ = app_handle.emit("mqtt-status", "connected");
                                    }
                                    Event::Incoming(Incoming::Publish(publish)) => {
                                        let topic = publish.topic.clone();
                                        let payload =
                                            String::from_utf8_lossy(&publish.payload).to_string();
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
                                let _ = app_handle
                                    .emit("mqtt-status", format!("error:{}", err_msg));
                                // 等待后重试，rumqttc eventloop 会自动尝试重连
                                tokio::time::sleep(Duration::from_secs(5)).await;
                            }
                        }
                    }
                }
            }
        });
        *self.loop_handle.lock().await = Some(handle);

        Ok("MQTT 连接中...".to_string())
    }

    /// FIX #8: 先 cancel → 等待事件循环退出 → 再 disconnect
    pub async fn disconnect(&self) -> Result<String, String> {
        if let Some(tx) = self.cancel_tx.lock().await.take() {
            let _ = tx.send(true);
        }
        // 等待事件循环任务退出（最多 2s）
        if let Some(handle) = self.loop_handle.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        }
        if let Some(client) = self.client.lock().await.take() {
            client.disconnect().await.map_err(|e| format!("断开失败: {}", e))?;
        }
        *self.status.lock().await = MqttStatus::Disconnected;
        Ok("MQTT 已断开".to_string())
    }

    pub async fn subscribe(&self, topic: &str) -> Result<String, String> {
        let client = {
            let guard = self.client.lock().await;
            guard.as_ref().ok_or("MQTT 未连接".to_string())?.clone()
        };
        client.subscribe(topic, QoS::AtLeastOnce).await.map_err(|e| format!("订阅失败: {}", e))?;
        Ok(format!("已订阅: {}", topic))
    }

    pub async fn publish(&self, topic: &str, payload: &str) -> Result<String, String> {
        let client = {
            let guard = self.client.lock().await;
            guard.as_ref().ok_or("MQTT 未连接".to_string())?.clone()
        };
        client
            .publish(topic, QoS::AtLeastOnce, false, payload.as_bytes())
            .await
            .map_err(|e| format!("发布失败: {}", e))?;
        Ok(format!("已发布到: {}", topic))
    }

    pub async fn get_status(&self) -> MqttStatus {
        self.status.lock().await.clone()
    }
}
