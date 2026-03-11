//! 投屏会话管理
//!
//! 管理多台设备的投屏会话，每台设备一个 Session。
//! 视频帧通过 Tauri Channel 直接推送到前端（避免 base64 + 全局事件广播）。

use super::control::ScrcpyControl;
use super::server::ScrcpyServer;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::ipc::{Channel, Response};
use tauri::Emitter;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

// ─── 常量 ─────────────────────────────────────────────

/// SG-3: 帧读取超时秒数（无帧则视为 server 挂死）
/// 注意：scrcpy 在屏幕静止时可能长时间不发送新帧，超时不能太短
const FRAME_READ_TIMEOUT_SECS: u64 = 120;

// ─── 事件载荷 ─────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct MirrorStartedPayload {
    pub serial: String,
    pub width: u32,
    pub height: u32,
}

// ─── 控制流 Actor 消息 ─────────────────────────────────────

enum ControlMsg {
    Touch { action: u8, x: u32, y: u32 },
    Key { keycode: u32, meta_state: u32 },
    Text(String),
    Back,
}

// ─── Session ─────────────────────────────────────────────

struct ScrcpySession {
    cancel: CancellationToken,
    /// C-2 修复：持有 pump 任务句柄，stop 时等待完成
    pump_handle: tokio::task::JoinHandle<()>,
    /// P0 优化：控制流 Actor 通道（替代 Arc<Mutex<TcpStream>>）
    control_tx: tokio::sync::mpsc::Sender<ControlMsg>,
}

/// 控制流 Actor：独占 TcpStream，串行发送，零锁竞争
async fn control_actor(
    mut stream: tokio::net::TcpStream,
    mut rx: tokio::sync::mpsc::Receiver<ControlMsg>,
    screen_w: u32,
    screen_h: u32,
) {
    while let Some(msg) = rx.recv().await {
        let result = match msg {
            ControlMsg::Touch { action, x, y } => {
                ScrcpyControl::inject_touch(&mut stream, action, x, y, screen_w, screen_h).await
            },
            ControlMsg::Key { keycode, meta_state } => {
                ScrcpyControl::inject_key(&mut stream, keycode, meta_state).await
            },
            ControlMsg::Text(text) => ScrcpyControl::inject_text(&mut stream, &text).await,
            ControlMsg::Back => ScrcpyControl::press_back(&mut stream).await,
        };
        if let Err(e) = result {
            eprintln!("[control-actor] 发送失败: {}", e);
            break;
        }
    }
}

// ─── SessionManager ─────────────────────────────────────────

pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, ScrcpySession>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self { sessions: Arc::new(RwLock::new(HashMap::new())) }
    }

    /// P0 修复：优雅关闭所有投屏会话（应用退出时调用）
    pub async fn shutdown(&self) {
        let sessions: HashMap<String, ScrcpySession> =
            std::mem::take(&mut *self.sessions.write().await);
        for (serial, session) in sessions {
            session.cancel.cancel();
            // 等待 pump 完成清理（pump 内部会调 server.stop()）
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(5), session.pump_handle).await;
            eprintln!("[scrcpy] shutdown: stopped {}", serial);
        }
    }

    /// 启动投屏
    pub async fn start_mirror(
        &self,
        serial: &str,
        jar_path: &str,
        app_handle: tauri::AppHandle,
        on_frame: Channel<Response>,
    ) -> Result<MirrorStartedPayload, String> {
        let mut sessions = self.sessions.write().await;
        if sessions.contains_key(serial) {
            return Err(format!("设备 {} 已在投屏中", serial));
        }

        let mut server = ScrcpyServer::start(serial, jar_path).await?;

        let screen_w = server.screen_width;
        let screen_h = server.screen_height;

        let video_stream = server.video_stream.take().ok_or("video stream 未建立")?;
        let control_stream = server.control_stream.take().ok_or("control stream 未建立")?;

        // P0 优化：启动控制流 Actor（bounded=32，try_send 背压丢帧）
        let (control_tx, control_rx) = tokio::sync::mpsc::channel::<ControlMsg>(32);
        tokio::spawn(control_actor(control_stream, control_rx, screen_w, screen_h));

        let cancel = CancellationToken::new();

        let serial_owned = serial.to_string();
        let cancel_clone = cancel.clone();
        let sessions_ref = Arc::clone(&self.sessions);

        // C-2 修复：保存 JoinHandle
        let pump_handle = tokio::spawn(async move {
            Self::frame_pump(
                serial_owned,
                video_stream,
                server,
                app_handle,
                on_frame,
                cancel_clone,
                sessions_ref,
            )
            .await;
        });

        sessions.insert(
            serial.to_string(),
            ScrcpySession { cancel: cancel.clone(), pump_handle, control_tx },
        );
        drop(sessions);

        Ok(MirrorStartedPayload { serial: serial.to_string(), width: screen_w, height: screen_h })
    }

    /// 停止投屏
    /// pump 可能已自行退出并清理，因此 session 不存在时不报错
    pub async fn stop_mirror(&self, serial: &str) -> Result<(), String> {
        let session = { self.sessions.write().await.remove(serial) };

        if let Some(session) = session {
            session.cancel.cancel();
            // 等待 pump 任务完成清理（最长 5s）
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(5), session.pump_handle).await;
            eprintln!("[scrcpy] 停止投屏: {}", serial);
        }
        // pump 可能已先于用户操作退出并清理，不报错
        Ok(())
    }

    /// 注入触控事件（P0 优化：单次 mpsc send，触控用 try_send 背压丢帧）
    pub async fn inject_touch(
        &self,
        serial: &str,
        action: u8,
        x: u32,
        y: u32,
    ) -> Result<(), String> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;

        // 高频 mousemove：try_send 满队列时丢弃，避免积压
        session
            .control_tx
            .try_send(ControlMsg::Touch { action, x, y })
            .map_err(|e| format!("控制消息发送失败: {}", e))
    }

    /// 注入按键事件
    pub async fn inject_key(
        &self,
        serial: &str,
        keycode: u32,
        meta_state: u32,
    ) -> Result<(), String> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;

        session
            .control_tx
            .send(ControlMsg::Key { keycode, meta_state })
            .await
            .map_err(|e| format!("控制消息发送失败: {}", e))
    }

    /// 注入文本（UTF-8 直传，支持中文等）
    pub async fn inject_text(&self, serial: &str, text: &str) -> Result<(), String> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;

        session
            .control_tx
            .send(ControlMsg::Text(text.to_string()))
            .await
            .map_err(|e| format!("控制消息发送失败: {}", e))
    }

    /// 注入返回键
    pub async fn press_back(&self, serial: &str) -> Result<(), String> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;

        session
            .control_tx
            .send(ControlMsg::Back)
            .await
            .map_err(|e| format!("控制消息发送失败: {}", e))
    }

    // ── 内部：帧推送循环 ──

    async fn frame_pump(
        serial: String,
        mut video_stream: tokio::net::TcpStream,
        mut server: ScrcpyServer,
        app_handle: tauri::AppHandle,
        channel: Channel<Response>,
        cancel: CancellationToken,
        sessions: Arc<RwLock<HashMap<String, ScrcpySession>>>,
    ) {
        eprintln!("[scrcpy] 帧推送启动: {}", serial);
        let start = std::time::Instant::now();
        // P1 优化：预分配帧缓冲池
        let mut pool = super::video::FramePool::new();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    eprintln!("[scrcpy] 帧推送取消: {}", serial);
                    break;
                }
                // SG-3: 帧读取加超时 — 120s 无帧视为 server 挂死
                result = tokio::time::timeout(
                    std::time::Duration::from_secs(FRAME_READ_TIMEOUT_SECS),
                    pool.read_frame_encoded(&mut video_stream, start.elapsed().as_millis() as u64),
                ) => {
                    match result {
                        Ok(Ok(Some(payload))) => {
                            // P2 优化：payload 已含 9B 头 + 帧数据（仅一次拷贝）
                            if channel.send(Response::new(payload)).is_err() {
                                eprintln!("[scrcpy] channel 已关闭: {}", serial);
                                break;
                            }
                        }
                        Ok(Ok(None)) => {
                            // 空帧，跳过
                            continue;
                        }
                        Ok(Err(e)) => {
                            eprintln!("[scrcpy] 帧读取失败: {} - {}", serial, e);
                            break;
                        }
                        Err(_) => {
                            eprintln!("[scrcpy] 帧读取超时 {}s，断开: {}", FRAME_READ_TIMEOUT_SECS, serial);
                            break;
                        }
                    }
                }
            }
        }

        server.stop().await;
        // 仅在 session 仍存在时移除（stop_mirror 可能已先移除）
        let was_present = sessions.write().await.remove(&serial).is_some();
        if was_present {
            let _ = app_handle.emit("scrcpy-stopped", &serial);
        }
        eprintln!("[scrcpy] 帧推送结束: {}", serial);
    }
}
