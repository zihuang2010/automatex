//! 真实 MQTT 连接管理器 — 基于 rumqttc 的完整实现

use crate::constants::{mqtt_emit_status, mqtt_topic, tauri_event};
use rumqttc::{AsyncClient, Event, Incoming, LastWill, MqttOptions, QoS};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::Mutex;

use super::router::route_message;
use super::types::{MqttConfig, MqttStatus};

/// 真实 MQTT 连接管理器
pub struct MqttManager {
    client: Arc<Mutex<Option<AsyncClient>>>,
    status: Arc<Mutex<MqttStatus>>,
    /// 取消标志：设为 true 时事件循环退出
    cancel_flag: Arc<AtomicBool>,
    loop_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    client_id: Arc<Mutex<String>>,
}

impl MqttManager {
    pub fn new() -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(MqttStatus::Disconnected)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            loop_handle: Arc::new(Mutex::new(None)),
            client_id: Arc::new(Mutex::new(String::new())),
        }
    }

    pub async fn connect(
        &self,
        config: MqttConfig,
        app_handle: tauri::AppHandle,
    ) -> Result<String, String> {
        // 先断开旧连接，确保旧事件循环完全退出
        self.disconnect().await.ok();

        *self.status.lock().await = MqttStatus::Connecting;
        *self.client_id.lock().await = config.client_id.clone();

        let mut opts = MqttOptions::new(&config.client_id, &config.broker_host, config.broker_port);
        opts.set_keep_alive(Duration::from_secs(crate::constants::timing::MQTT_KEEP_ALIVE_SECS));
        opts.set_clean_session(true);

        if let (Some(ref user), Some(ref pass)) = (&config.username, &config.password) {
            if !user.is_empty() {
                opts.set_credentials(user, pass);
            }
        }

        // LWT 遗嘱消息
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

        // 重置取消标志
        self.cancel_flag.store(false, Ordering::SeqCst);
        let cancel = Arc::clone(&self.cancel_flag);

        let status = self.status.clone();
        let cid = config.client_id.clone();

        let handle = tokio::spawn(async move {
            let mut connect_ts: i64 = crate::constants::now_unix();
            let mut backoff_secs: u64 = 5;
            let mut was_connected = false;

            loop {
                // ── 检查取消标志（在 poll 之前，不与 poll 并发执行） ──
                if cancel.load(Ordering::SeqCst) {
                    *status.lock().await = MqttStatus::Disconnected;
                    eprintln!("[mqtt] 收到取消信号，退出事件循环");
                    break;
                }

                // ── 驱动 eventloop ──
                // 不使用 tokio::select!，避免 drop poll future 导致 rumqttc 内部状态损坏
                // 使用超时包裹确保 cancel_flag 能被及时检测
                let poll_result =
                    tokio::time::timeout(Duration::from_secs(1), eventloop.poll()).await;

                match poll_result {
                    Err(_) => {
                        // poll 超时（1秒），正常：回到循环顶部检查 cancel_flag
                        continue;
                    },
                    Ok(Ok(event)) => {
                        backoff_secs = 5;
                        match &event {
                            Event::Incoming(Incoming::ConnAck(_)) => {
                                was_connected = true;
                                *status.lock().await = MqttStatus::Connected;
                                let _ = app_handle
                                    .emit(tauri_event::MQTT_STATUS, mqtt_emit_status::CONNECTED);

                                connect_ts = crate::constants::now_unix();

                                let sub_downstream =
                                    mqtt_topic::client_topic(&cid, mqtt_topic::DOWN_WILDCARD);
                                let sub_broadcast =
                                    mqtt_topic::broadcast_topic(mqtt_topic::BROADCAST_WILDCARD);

                                if let Err(e) =
                                    client.subscribe(&sub_downstream, QoS::AtLeastOnce).await
                                {
                                    eprintln!("[mqtt] 订阅 downstream 失败: {}", e);
                                } else {
                                    eprintln!("[mqtt] 已订阅: {}", sub_downstream);
                                }
                                if let Err(e) =
                                    client.subscribe(&sub_broadcast, QoS::AtLeastOnce).await
                                {
                                    eprintln!("[mqtt] 订阅 broadcast 失败: {}", e);
                                } else {
                                    eprintln!("[mqtt] 已订阅: {}", sub_broadcast);
                                }

                                eprintln!("[mqtt] 连接成功，connect_ts={}", connect_ts);
                            },
                            Event::Incoming(Incoming::Publish(publish)) => {
                                let topic = publish.topic.clone();
                                let payload = String::from_utf8_lossy(&publish.payload).to_string();
                                route_message(&topic, &payload, &app_handle, connect_ts);
                            },
                            Event::Incoming(Incoming::Disconnect) => {
                                was_connected = false;
                                *status.lock().await = MqttStatus::Disconnected;
                                let _ = app_handle
                                    .emit(tauri_event::MQTT_STATUS, mqtt_emit_status::DISCONNECTED);
                            },
                            _ => {},
                        }
                    },
                    Ok(Err(e)) => {
                        let err_msg = format!("{}", e);

                        if was_connected {
                            // 曾经连接成功 → 瞬态断线，rumqttc 会自动重连
                            *status.lock().await = MqttStatus::Connecting;
                            let _ = app_handle
                                .emit(tauri_event::MQTT_STATUS, mqtt_emit_status::CONNECTING);
                            eprintln!("[mqtt] 连接中断，自动重连中: {}", err_msg);
                            was_connected = false;
                        } else {
                            // 从未成功连接过 → 真正的连接错误
                            *status.lock().await = MqttStatus::Error(err_msg.clone());
                            let _ = app_handle
                                .emit(tauri_event::MQTT_STATUS, format!("error:{}", err_msg));
                            eprintln!("[mqtt] 连接错误，{}s 后重试: {}", backoff_secs, err_msg);
                        }

                        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(60);
                    },
                }
            }
        });
        *self.loop_handle.lock().await = Some(handle);

        Ok("MQTT 连接中...".to_string())
    }

    /// 先设置 cancel flag → disconnect → 等待事件循环退出
    pub async fn disconnect(&self) -> Result<String, String> {
        // 先设取消标志，让事件循环尽快退出
        self.cancel_flag.store(true, Ordering::SeqCst);

        if let Some(client) = self.client.lock().await.take() {
            let _ = client.disconnect().await;
        }
        if let Some(handle) = self.loop_handle.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
        }
        *self.status.lock().await = MqttStatus::Disconnected;
        Ok("MQTT 已断开".to_string())
    }

    pub async fn subscribe(&self, topic: &str) -> Result<String, String> {
        let client = {
            let guard = self.client.lock().await;
            guard.as_ref().ok_or("MQTT 未连接".to_string())?.clone()
        };
        client
            .subscribe(topic, QoS::AtLeastOnce)
            .await
            .map_err(|e| format!("订阅失败: {}", e))?;
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
            "ts": crate::constants::now_unix(),
            "devices": device_hw_serials,
            "tasks_executing": tasks_executing,
        })
        .to_string();
        self.publish(&topic, &payload).await?;
        Ok(())
    }

    pub async fn get_status(&self) -> MqttStatus {
        self.status.lock().await.clone()
    }
}
