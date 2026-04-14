//! 真实 MQTT 连接管理器 — 基于 rumqttc 的完整实现
//!
//! # 并发安全设计
//!
//! ## 核心问题：cancel_flag 共享竞态
//! 若使用共享 `AtomicBool` cancel_flag，connect() 在 disconnect() 返回后
//! 立刻 store(false) 会与仍在运行的旧 EventLoop 产生竞态：
//!   旧loop见到 cancel=true → 准备退出 → connect() store(false) → 旧loop继续跑
//!
//! ## 解决方案：generation 代数计数器
//! 每次 connect() 将全局 generation +1，并把当代代数传入新 EventLoop。
//! EventLoop 在每轮循环开头对比自己持有的代数与当前全局代数：
//!   - 相等 → 继续运行（自己是最新一代）
//!   - 不等 → 立刻退出（已被新连接替代）
//!
//! connect() 永远不需要 store(false)，彻底消除竞态。
//!
//! ## 重连策略：每次断线创建全新 EventLoop
//! 依赖 rumqttc 内部自动重连会导致旧 session 中未 ACK 的 QoS 1 数据包（outbox）
//! 在新会话中被重发（DUP=1）。Broker 在 clean_session=true 下没有对应记录，
//! 会直接关闭 TCP 连接（"connection closed by peer"），形成无限重连循环。
//! 解法：每次断线都使用外层 'reconnect 循环重建 (client, eventloop)，彻底清空 outbox。

use crate::constants::{mqtt_emit_status, mqtt_topic, tauri_event};
use rumqttc::{AsyncClient, Event, Incoming, LastWill, MqttOptions, QoS};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;
use tokio::sync::Mutex;

use tracing::{error, info, warn};

use super::router::route_message;
use super::types::*;

/// 当前时间格式化为 "yyyy-MM-dd HH:mm:ss"
fn now_event_at() -> String {
    crate::utils::format_datetime(crate::constants::now_unix())
}

/// 真实 MQTT 连接管理器
pub struct MqttManager {
    client: Arc<Mutex<Option<AsyncClient>>>,
    status: Arc<Mutex<MqttStatus>>,
    /// 代数计数器：每次 connect() +1，旧 EventLoop 检测到代数不符时立刻退出
    /// 彻底替代 AtomicBool cancel_flag，消除 store(false) 竞态
    generation: Arc<AtomicU64>,
    loop_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    client_id: Arc<Mutex<String>>,
}

impl MqttManager {
    pub fn new() -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(MqttStatus::Disconnected)),
            generation: Arc::new(AtomicU64::new(0)),
            loop_handle: Arc::new(Mutex::new(None)),
            client_id: Arc::new(Mutex::new(String::new())),
        }
    }

    pub async fn connect(
        &self,
        config: MqttConfig,
        app_handle: tauri::AppHandle,
    ) -> Result<String, String> {
        // ── Step 1: 让旧 EventLoop 自然退出 ──
        let my_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        if let Some(old_client) = self.client.lock().await.take() {
            let _ = old_client.disconnect().await;
        }
        if let Some(old_handle) = self.loop_handle.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), old_handle).await;
        }

        // ── Step 2: 设置状态 ──
        *self.status.lock().await = MqttStatus::Connecting;
        *self.client_id.lock().await = config.client_id.clone();
        let _ = app_handle.emit(tauri_event::MQTT_STATUS, mqtt_emit_status::CONNECTING);

        // 连接尝试日志：出现 NotAuthorized / 认证失败时可直接从日志核对实际参数
        // （密码仅输出是否存在，避免泄露）
        info!(
            generation = my_generation,
            broker = %config.broker_host,
            port = config.broker_port,
            client_id = %config.client_id,
            username = %config.username.as_deref().unwrap_or("<NONE>"),
            has_password = config.password.is_some(),
            "MQTT 连接尝试"
        );

        // ── Step 3: 启动重连外循环，每次断线都创建全新 (client, eventloop) ──
        let generation_arc = Arc::clone(&self.generation);
        let status = self.status.clone();
        let client_arc = self.client.clone(); // 新建连接后更新，供 publish/subscribe 使用

        let handle = tokio::spawn(async move {
            let mut backoff_secs: u64 = 5;

            'reconnect: loop {
                // ── 代数守卫 ──
                if generation_arc.load(Ordering::SeqCst) != my_generation {
                    info!(generation = my_generation, "代数已被替换，EventLoop 退出");
                    break 'reconnect;
                }

                // ── 每次重连都构建全新 opts + (client, eventloop)，彻底清空旧 outbox ──
                let mut opts =
                    MqttOptions::new(&config.client_id, &config.broker_host, config.broker_port);
                opts.set_keep_alive(Duration::from_secs(
                    crate::constants::timing::MQTT_KEEP_ALIVE_SECS,
                ));
                opts.set_clean_session(true);

                if let (Some(ref user), Some(ref pass)) = (&config.username, &config.password) {
                    if !user.is_empty() {
                        opts.set_credentials(user, pass);
                    }
                }

                let will_topic =
                    mqtt_topic::client_topic(&config.client_id, mqtt_topic::UP_OFFLINE);
                let will_payload = serde_json::to_string(&MsgLwt {
                    client_id: &config.client_id,
                    reason: "unexpected_disconnect",
                })
                .unwrap();
                opts.set_last_will(LastWill::new(
                    will_topic,
                    will_payload.into_bytes(),
                    QoS::AtLeastOnce,
                    false,
                ));

                let (client, mut eventloop) = AsyncClient::new(opts, 100);
                eventloop.network_options.set_connection_timeout(8);

                // 更新外部可访问的 client（publish_heartbeat / subscribe 会用到）
                *client_arc.lock().await = Some(client.clone());

                let mut connect_ts: i64 = crate::constants::now_unix();
                let mut was_connected = false;

                // ── 内层 poll 循环：只负责驱动当前 EventLoop ──
                'poll: loop {
                    if generation_arc.load(Ordering::SeqCst) != my_generation {
                        break 'reconnect;
                    }

                    match eventloop.poll().await {
                        Ok(event) => {
                            backoff_secs = 5; // 成功收到事件，重置退避
                            match &event {
                                Event::Incoming(Incoming::ConnAck(_)) => {
                                    was_connected = true;
                                    *status.lock().await = MqttStatus::Connected;
                                    let _ = app_handle.emit(
                                        tauri_event::MQTT_STATUS,
                                        mqtt_emit_status::CONNECTED,
                                    );
                                    connect_ts = crate::constants::now_unix();

                                    // ⚠️ subscribe 必须在独立 task 内执行，避免自我死锁
                                    // 带指数退避重试，防止订阅失败后静默丢失下行消息
                                    let client_sub = client.clone();
                                    let sub_downstream = mqtt_topic::client_topic(
                                        &config.client_id,
                                        mqtt_topic::DOWN_WILDCARD,
                                    );
                                    let sub_broadcast =
                                        mqtt_topic::broadcast_topic(mqtt_topic::BROADCAST_WILDCARD);
                                    let gen_for_sub = my_generation;
                                    let gen_arc_sub = Arc::clone(&generation_arc);
                                    tokio::spawn(async move {
                                        let topics = [sub_downstream, sub_broadcast];
                                        for topic in &topics {
                                            let mut ok = false;
                                            for attempt in 0..5u32 {
                                                if gen_arc_sub.load(Ordering::SeqCst) != gen_for_sub
                                                {
                                                    return; // 代数已变，放弃
                                                }
                                                match client_sub
                                                    .subscribe(topic.as_str(), QoS::AtLeastOnce)
                                                    .await
                                                {
                                                    Ok(_) => {
                                                        info!(topic = %topic, "已订阅");
                                                        ok = true;
                                                        break;
                                                    },
                                                    Err(e) => {
                                                        let delay = Duration::from_millis(
                                                            500 * 2u64.pow(attempt),
                                                        );
                                                        warn!(
                                                            topic = %topic,
                                                            attempt = attempt + 1,
                                                            error = %e,
                                                            "订阅失败，{:?} 后重试", delay
                                                        );
                                                        tokio::time::sleep(delay).await;
                                                    },
                                                }
                                            }
                                            if !ok {
                                                error!(topic = %topic, "订阅重试耗尽");
                                            }
                                        }
                                    });

                                    // 发布上线事件
                                    let client_online = client.clone();
                                    let online_topic = mqtt_topic::client_topic(
                                        &config.client_id,
                                        mqtt_topic::UP_DEVICE_ONLINE,
                                    );
                                    let online_payload = serde_json::to_string(&MsgOnline {
                                        client_id: &config.client_id,
                                        event_at: now_event_at(),
                                    })
                                    .unwrap();
                                    tokio::spawn(async move {
                                        if let Err(e) = client_online
                                            .publish(
                                                online_topic,
                                                QoS::AtLeastOnce,
                                                false,
                                                online_payload.into_bytes(),
                                            )
                                            .await
                                        {
                                            error!(error = %e, "上线事件发布失败");
                                        }
                                    });

                                    info!(
                                        generation = my_generation,
                                        connect_ts = connect_ts,
                                        "连接成功"
                                    );
                                },
                                Event::Incoming(Incoming::Publish(publish)) => {
                                    let topic = publish.topic.clone();
                                    let payload =
                                        String::from_utf8_lossy(&publish.payload).to_string();
                                    route_message(
                                        &topic,
                                        &payload,
                                        &app_handle,
                                        &config.client_id,
                                        connect_ts,
                                    );
                                },
                                Event::Incoming(Incoming::Disconnect) => {
                                    // Broker 主动发 DISCONNECT → 等待片刻后重新建立连接
                                    *status.lock().await = MqttStatus::Connecting;
                                    let _ = app_handle.emit(
                                        tauri_event::MQTT_STATUS,
                                        mqtt_emit_status::CONNECTING,
                                    );
                                    warn!("Broker 主动断开，3s 后重连");
                                    tokio::time::sleep(Duration::from_secs(3)).await;
                                    break 'poll; // 退出内层，外层用全新 EventLoop 重连
                                },
                                _ => {},
                            }
                        },
                        Err(e) => {
                            let err_msg = format!("{}", e);

                            if was_connected {
                                // 曾经连接成功 → 瞬态断线，用全新 EventLoop 重连（清除旧 outbox）
                                *status.lock().await = MqttStatus::Connecting;
                                let _ = app_handle
                                    .emit(tauri_event::MQTT_STATUS, mqtt_emit_status::CONNECTING);
                                warn!(error = %err_msg, "连接中断，3s 后重连");
                                tokio::time::sleep(Duration::from_secs(3)).await;
                                backoff_secs = 5; // 曾经成功过，重置退避计数
                            } else {
                                // 从未成功连接过 → 真正的连接错误，指数退避
                                *status.lock().await = MqttStatus::Error(err_msg.clone());
                                let _ = app_handle
                                    .emit(tauri_event::MQTT_STATUS, format!("error:{}", err_msg));
                                error!(error = %err_msg, backoff_secs = backoff_secs, "连接错误，稍后重试");
                                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                                backoff_secs = (backoff_secs * 2).min(60);
                            }

                            break 'poll; // 退出内层，外层用全新 EventLoop 重连
                        },
                    }
                }
                // 'poll 循环结束后自动进入下一次 'reconnect 迭代
            }
        });

        *self.loop_handle.lock().await = Some(handle);
        Ok("MQTT 连接中...".to_string())
    }

    /// 断开连接：发送下线事件 → 递增代数 → 断开 TCP → 等待 loop 退出
    pub async fn disconnect(&self) -> Result<String, String> {
        // 先发送下线事件（在 take 之前 borrow client）
        {
            let cid = self.client_id.lock().await.clone();
            if !cid.is_empty() {
                let guard = self.client.lock().await;
                if let Some(client) = guard.as_ref() {
                    let topic = mqtt_topic::client_topic(&cid, mqtt_topic::UP_DEVICE_OFFLINE);
                    let payload = serde_json::to_string(&MsgOffline {
                        client_id: &cid,
                        event_at: now_event_at(),
                        reason: "user_logout",
                    })
                    .unwrap();
                    // 尽力发送，超时不阻塞
                    let _ = tokio::time::timeout(
                        Duration::from_secs(2),
                        client.publish(topic, QoS::AtLeastOnce, false, payload.into_bytes()),
                    )
                    .await;
                }
            }
        }

        self.generation.fetch_add(1, Ordering::SeqCst);

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

    /// 发布心跳（使用 QoS 0，心跳是时效性数据，无需保证投递，避免重连时 DUP 重发）
    pub async fn publish_heartbeat(&self, device_hw_serials: Vec<String>) -> Result<(), String> {
        let cid = self.client_id.lock().await.clone();
        if cid.is_empty() {
            return Ok(());
        }
        let topic = mqtt_topic::client_topic(&cid, mqtt_topic::UP_HEARTBEAT);
        let payload = serde_json::to_string(&MsgHeartbeat {
            client_id: &cid,
            event_at: now_event_at(),
            devices: &device_hw_serials,
        })
        .unwrap();

        let client = {
            let guard = self.client.lock().await;
            guard.as_ref().ok_or("MQTT 未连接".to_string())?.clone()
        };
        client
            .publish(topic, QoS::AtMostOnce, false, payload.as_bytes()) // QoS 0：心跳不需要 ACK
            .await
            .map_err(|e| format!("心跳发布失败: {}", e))?;
        Ok(())
    }

    /// 发布任务状态事件（fire-and-forget，失败仅记录日志）
    ///
    /// `event_type`: started / paused / continue / stopped / restart
    pub async fn publish_task_event(&self, task_id: &str, event_type: &str) {
        let cid = self.client_id.lock().await.clone();
        if cid.is_empty() {
            return;
        }
        let topic = mqtt_topic::client_topic(&cid, mqtt_topic::UP_TASK_EVENT);
        let payload = serde_json::to_string(&MsgTaskEvent {
            client_id: &cid,
            task_id,
            event_type,
            event_at: now_event_at(),
        })
        .unwrap();

        let client = {
            let guard = self.client.lock().await;
            match guard.as_ref() {
                Some(c) => c.clone(),
                None => return,
            }
        };
        if let Err(e) = client.publish(topic, QoS::AtLeastOnce, false, payload.into_bytes()).await {
            error!(
                task_id = task_id,
                event_type = event_type,
                error = %e,
                "任务事件发布失败"
            );
        }
    }

    pub async fn get_status(&self) -> MqttStatus {
        self.status.lock().await.clone()
    }
}
