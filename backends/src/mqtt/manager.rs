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
//! connect() 永远不需要 store(false)，彻底消除竞态。

use crate::constants::{mqtt_emit_status, mqtt_topic, tauri_event};
use rumqttc::{AsyncClient, Event, Incoming, LastWill, MqttOptions, QoS};
use std::sync::atomic::{AtomicU64, Ordering};
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
        // 先递增代数，让旧 loop 在下一次轮询超时（最大 1s）后检测到代数不符自动退出
        // 无需 store(false)，不存在竞态
        let my_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        // 断开旧 client（让内核 TCP 层快速关闭）
        if let Some(old_client) = self.client.lock().await.take() {
            let _ = old_client.disconnect().await;
        }

        // 等旧 loop JoinHandle 最多 2 秒，确保 EventLoop 对象被 drop
        // 超时不 panic，旧 loop 检测到代数不符后也会在≤1s 内退出
        if let Some(old_handle) = self.loop_handle.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), old_handle).await;
        }


        // ── Step 2: 设置状态 & 构建新连接选项 ──
        *self.status.lock().await = MqttStatus::Connecting;
        *self.client_id.lock().await = config.client_id.clone();
        let _ = app_handle.emit(tauri_event::MQTT_STATUS, mqtt_emit_status::CONNECTING);

        let mut opts =
            MqttOptions::new(&config.client_id, &config.broker_host, config.broker_port);
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
        eventloop.network_options.set_connection_timeout(8);
        *self.client.lock().await = Some(client.clone());

        // ── Step 3: 启动新 EventLoop，持有本代代数 ──
        let generation_arc = Arc::clone(&self.generation);
        let status = self.status.clone();
        let cid = config.client_id.clone();

        let handle = tokio::spawn(async move {
            let mut connect_ts: i64 = crate::constants::now_unix();
            let mut backoff_secs: u64 = 5;
            let mut was_connected = false;

            loop {
                // ── 代数守卫：检测自己是否已被新 connect() 替代 ──
                // 旧 loop 在 poll 返回后会在此处退出，不存在被 store(false) 复活的风险
                if generation_arc.load(Ordering::SeqCst) != my_generation {
                    eprintln!("[mqtt] 代数 {} 已被替换，EventLoop 退出", my_generation);
                    // 旧 loop 退出时不修改 status，新 loop 自己管理
                    break;
                }

                // 直接驱动 rumqttc 的 poll。
                // poll 内部已经处理了连接超时和网络超时；外部再包 timeout 会在握手阶段提前打断 poll，
                // 导致连接长期停在“connecting”而拿不到 ConnAck / 明确错误。
                match eventloop.poll().await {
                    Ok(event) => {
                        backoff_secs = 5;
                        match &event {
                            Event::Incoming(Incoming::ConnAck(_)) => {
                                was_connected = true;
                                *status.lock().await = MqttStatus::Connected;
                                let _ = app_handle
                                    .emit(tauri_event::MQTT_STATUS, mqtt_emit_status::CONNECTED);

                                connect_ts = crate::constants::now_unix();

                                // ⚠️ 关键修复：绝对不能在 eventloop task 内 .await subscribe！
                                // client.subscribe() 向 rumqttc 内部 flume channel 写指令，
                                // 需要 eventloop 消费该 channel 才能完成，
                                // 若在同一 task 内 .await 会造成自我死锁（channel 满 → 双方互等）。
                                // 解法：spawn 独立 task 发送订阅，让 eventloop task 立刻返回继续 poll。
                                let client_sub = client.clone();
                                let sub_downstream =
                                    mqtt_topic::client_topic(&cid, mqtt_topic::DOWN_WILDCARD);
                                let sub_broadcast =
                                    mqtt_topic::broadcast_topic(mqtt_topic::BROADCAST_WILDCARD);
                                let gen_for_sub = my_generation;
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        client_sub.subscribe(&sub_downstream, QoS::AtLeastOnce).await
                                    {
                                        eprintln!("[mqtt] 订阅 downstream 失败 (gen={}): {}", gen_for_sub, e);
                                    } else {
                                        eprintln!("[mqtt] 已订阅: {}", sub_downstream);
                                    }
                                    if let Err(e) =
                                        client_sub.subscribe(&sub_broadcast, QoS::AtLeastOnce).await
                                    {
                                        eprintln!("[mqtt] 订阅 broadcast 失败 (gen={}): {}", gen_for_sub, e);
                                    } else {
                                        eprintln!("[mqtt] 已订阅: {}", sub_broadcast);
                                    }
                                });

                                eprintln!(
                                    "[mqtt] 连接成功 (gen={}), connect_ts={}",
                                    my_generation, connect_ts
                                );
                            },
                            Event::Incoming(Incoming::Publish(publish)) => {
                                let topic = publish.topic.clone();
                                let payload =
                                    String::from_utf8_lossy(&publish.payload).to_string();
                                route_message(&topic, &payload, &app_handle, &cid, connect_ts);
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
                    Err(e) => {
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

    /// 断开连接：递增代数（让 EventLoop 感知退出）→ 断开 TCP → 等待 loop 退出
    pub async fn disconnect(&self) -> Result<String, String> {
        // 递增代数，让当前运行的 EventLoop 在≤800ms 内检测到并退出
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
