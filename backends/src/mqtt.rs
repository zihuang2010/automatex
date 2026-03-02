use crate::constants::mqtt_topic;
use rumqttc::{AsyncClient, Event, Incoming, LastWill, MqttOptions, QoS};
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
    cancel_tx: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>>,
    loop_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    client_id: Arc<Mutex<String>>,
}

impl MqttManager {
    pub fn new() -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(MqttStatus::Disconnected)),
            cancel_tx: Arc::new(Mutex::new(None)),
            loop_handle: Arc::new(Mutex::new(None)),
            client_id: Arc::new(Mutex::new(String::new())),
        }
    }

    pub async fn connect(
        &self,
        config: MqttConfig,
        app_handle: tauri::AppHandle,
    ) -> Result<String, String> {
        self.disconnect().await.ok();

        *self.status.lock().await = MqttStatus::Connecting;
        *self.client_id.lock().await = config.client_id.clone();

        let mut opts = MqttOptions::new(&config.client_id, &config.broker_host, config.broker_port);
        opts.set_keep_alive(Duration::from_secs(crate::constants::timing::MQTT_KEEP_ALIVE_SECS));

        if let (Some(ref user), Some(ref pass)) = (&config.username, &config.password) {
            if !user.is_empty() {
                opts.set_credentials(user, pass);
            }
        }

        // ── LWT 遗嘱消息 ──
        // 客户端异常断开时，Broker 自动发布此消息
        let will_topic = mqtt_topic::client_topic(&config.client_id, mqtt_topic::UP_OFFLINE);
        let will_payload = serde_json::json!({
            "client_id": config.client_id,
            "reason": "unexpected_disconnect"
        })
        .to_string();
        opts.set_last_will(LastWill::new(
            will_topic,
            will_payload.into_bytes(),
            QoS::AtLeastOnce,
            false,
        ));

        let (client, mut eventloop) = AsyncClient::new(opts, 100);
        *self.client.lock().await = Some(client.clone());

        let status = self.status.clone();
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        *self.cancel_tx.lock().await = Some(cancel_tx);

        let cid = config.client_id.clone();

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

                                        // ── 自动订阅下行 Topic ──
                                        let sub_downstream = mqtt_topic::client_topic(&cid, mqtt_topic::DOWN_WILDCARD);
                                        let sub_broadcast = mqtt_topic::broadcast_topic(mqtt_topic::BROADCAST_WILDCARD);

                                        if let Err(e) = client.subscribe(&sub_downstream, QoS::AtLeastOnce).await {
                                            eprintln!("[mqtt] 订阅 downstream 失败: {}", e);
                                        } else {
                                            eprintln!("[mqtt] 已订阅: {}", sub_downstream);
                                        }
                                        if let Err(e) = client.subscribe(&sub_broadcast, QoS::AtLeastOnce).await {
                                            eprintln!("[mqtt] 订阅 broadcast 失败: {}", e);
                                        } else {
                                            eprintln!("[mqtt] 已订阅: {}", sub_broadcast);
                                        }
                                    }
                                    Event::Incoming(Incoming::Publish(publish)) => {
                                        let topic = publish.topic.clone();
                                        let payload = String::from_utf8_lossy(&publish.payload).to_string();

                                        // ── 按 Topic 路由到不同事件 ──
                                        route_message(&topic, &payload, &app_handle);
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

    /// 先 disconnect（发送 DISCONNECT 包）→ cancel → 等待事件循环退出
    pub async fn disconnect(&self) -> Result<String, String> {
        if let Some(client) = self.client.lock().await.take() {
            let _ = client.disconnect().await;
        }
        if let Some(tx) = self.cancel_tx.lock().await.take() {
            let _ = tx.send(true);
        }
        if let Some(handle) = self.loop_handle.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
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

    // ─── 业务发布方法 ─────────────────────────────────────────────

    /// 发布设备上线事件
    pub async fn publish_device_online(
        &self,
        hw_serial: &str,
        serial: &str,
        name: &str,
    ) -> Result<(), String> {
        let cid = self.client_id.lock().await.clone();
        if cid.is_empty() {
            return Ok(()); // 未连接，静默跳过
        }
        let topic = mqtt_topic::client_topic(&cid, mqtt_topic::UP_DEVICE_ONLINE);
        let payload = serde_json::json!({
            "ts": now_unix(),
            "hw_serial": hw_serial,
            "serial": serial,
            "name": name,
        })
        .to_string();
        self.publish(&topic, &payload).await?;
        Ok(())
    }

    /// 发布设备下线事件
    pub async fn publish_device_offline(
        &self,
        hw_serial: &str,
        serial: &str,
    ) -> Result<(), String> {
        let cid = self.client_id.lock().await.clone();
        if cid.is_empty() {
            return Ok(());
        }
        let topic = mqtt_topic::client_topic(&cid, mqtt_topic::UP_DEVICE_OFFLINE);
        let payload = serde_json::json!({
            "ts": now_unix(),
            "hw_serial": hw_serial,
            "serial": serial,
        })
        .to_string();
        self.publish(&topic, &payload).await?;
        Ok(())
    }

    /// 发布心跳（包含在线设备 hw_serial 列表和执行中任务列表）
    pub async fn publish_heartbeat(
        &self,
        device_hw_serials: Vec<String>,
        tasks_executing: Vec<String>,
    ) -> Result<(), String> {
        let cid = self.client_id.lock().await.clone();
        if cid.is_empty() {
            return Ok(());
        }
        let topic = mqtt_topic::client_topic(&cid, mqtt_topic::UP_HEARTBEAT);
        let payload = serde_json::json!({
            "ts": now_unix(),
            "devices": device_hw_serials,
            "tasks_executing": tasks_executing,
        })
        .to_string();
        self.publish(&topic, &payload).await?;
        Ok(())
    }

    /// 发布任务状态事件
    pub async fn publish_task_event(
        &self,
        task_id: &str,
        event: &str,
        device_serial: &str,
        detail: &str,
    ) -> Result<(), String> {
        let cid = self.client_id.lock().await.clone();
        if cid.is_empty() {
            return Ok(());
        }
        let topic = mqtt_topic::client_topic(&cid, mqtt_topic::UP_TASK_EVENT);
        let payload = serde_json::json!({
            "ts": now_unix(),
            "task_id": task_id,
            "event": event,
            "device_serial": device_serial,
            "detail": detail,
        })
        .to_string();
        self.publish(&topic, &payload).await?;
        Ok(())
    }

    pub async fn get_status(&self) -> MqttStatus {
        self.status.lock().await.clone()
    }
}

// ─── 消息路由 ─────────────────────────────────────────────

/// 根据 Topic 后缀将消息路由到不同的 Tauri 前端事件
fn route_message(topic: &str, payload: &str, app_handle: &tauri::AppHandle) {
    // 解析 JSON payload
    let json_value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[mqtt] 消息 JSON 解析失败: topic={}, err={}", topic, e);
            return;
        },
    };

    if topic.ends_with(mqtt_topic::DOWN_DEVICE_KICK) {
        // ── 踢设备下线 ──
        eprintln!("[mqtt] 收到踢设备指令: {}", payload);
        let _ = app_handle.emit("mqtt-device-kick", json_value);
    } else if topic.ends_with(mqtt_topic::DOWN_TASK_RELOAD) {
        // ── 任务数据变更 ──
        eprintln!("[mqtt] 收到任务变更通知: {}", payload);
        let _ = app_handle.emit("mqtt-task-reload", json_value);
    } else if topic.contains(mqtt_topic::BROADCAST_TASK_UPDATE) {
        // ── 全局任务广播 ──
        eprintln!("[mqtt] 收到全局任务广播: {}", payload);
        let _ = app_handle.emit("mqtt-task-reload", json_value);
    } else {
        // 未知 Topic，转发到通用事件
        let _ = app_handle.emit(
            "mqtt-message",
            serde_json::json!({
                "topic": topic,
                "payload": payload,
            }),
        );
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
        as i64
}
