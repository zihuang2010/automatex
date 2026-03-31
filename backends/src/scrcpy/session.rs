//! 投屏会话管理
//!
//! 管理多台设备的投屏会话，每台设备一个 Session。
//! 视频帧通过 Tauri Channel 直接推送到前端（避免 base64 + 全局事件广播）。

use crate::{connection::adb, constants};
use super::control::{DeviceMessage, ScrcpyControl};
use super::server::ScrcpyServer;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::ipc::{Channel, Response};
use tauri::Emitter;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

// ─── 常量 ─────────────────────────────────────────────

/// SG-3: 帧读取超时秒数（无帧则视为 server 挂死）
/// 注意：scrcpy 在屏幕静止时可能长时间不发送新帧，超时不能太短
const FRAME_READ_TIMEOUT_SECS: u64 = 120;
const CONTROL_WRITE_TIMEOUT_SECS: u64 = 5;
const CLIPBOARD_ACK_TIMEOUT_MS: u64 = 1500;
const CLIPBOARD_ECHO_GUARD_TTL_SECS: u64 = 10;
const NON_ASCII_PASTE_CHUNK_BYTES: usize = 2048;
const NON_ASCII_PASTE_CHUNK_DELAY_MS: u64 = 24;
const FIRST_FRAME_STATE_EMIT_DEBOUNCE_MS: u64 = 250;

type ClipboardAckMap = Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>>;
type ClipboardEchoGuards = Arc<Mutex<VecDeque<ClipboardEchoGuard>>>;

// ─── 事件载荷 ─────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct MirrorStartedPayload {
    pub serial: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextRoute {
    AsciiDirect,
    AdbImeText,
    ClipboardFallback,
}

impl TextRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AsciiDirect => "ascii_direct",
            Self::AdbImeText => "adb_ime_text",
            Self::ClipboardFallback => "clipboard_fallback",
        }
    }
}

#[derive(Clone, Serialize)]
pub struct ScrcpyTextRoutePayload {
    pub serial: String,
    pub route: &'static str,
}

#[derive(Clone, Serialize)]
pub struct ScrcpySessionStatePayload {
    pub serial: String,
    pub phase: &'static str,
    pub width: u32,
    pub height: u32,
    pub recover_reason: Option<String>,
    pub last_frame_at_ms: Option<u64>,
}

#[derive(Clone, Serialize)]
struct ClipboardSyncPayload {
    serial: String,
    text: String,
}

#[derive(Clone, Copy)]
struct ClipboardEchoGuard {
    hash: u64,
    len: usize,
    expires_at: Instant,
}

// ─── 控制流 Actor 消息 ─────────────────────────────────────

enum ControlMsg {
    Touch { action: u8, x: u32, y: u32 },
    Scroll { x: u32, y: u32, h_scroll: f32, v_scroll: f32, buttons: u32 },
    Key { keycode: u32, meta_state: u32 },
    Text(String),
    Back,
    ResetVideo,
}

// ─── Session ─────────────────────────────────────────────

struct ScrcpySession {
    cancel: CancellationToken,
    screen_width: u32,
    screen_height: u32,
    /// C-2 修复：持有 pump 任务句柄，stop 时等待完成
    pump_handle: tokio::task::JoinHandle<()>,
    /// P0 优化：控制流 Actor 通道（替代 Arc<Mutex<TcpStream>>）
    control_tx: tokio::sync::mpsc::Sender<ControlMsg>,
    control_handle: tokio::task::JoinHandle<()>,
    control_reader_handle: tokio::task::JoinHandle<()>,
}

/// 控制流 Actor：独占 TcpStream，串行发送，零锁竞争
async fn control_actor(
    mut stream: OwnedWriteHalf,
    mut rx: tokio::sync::mpsc::Receiver<ControlMsg>,
    pending_acks: ClipboardAckMap,
    local_clipboards: ClipboardEchoGuards,
    next_clipboard_sequence: Arc<AtomicU64>,
    screen_w: u32,
    screen_h: u32,
    cancel: CancellationToken,
) {
    while let Some(msg) = rx.recv().await {
        let result = match msg {
            ControlMsg::Touch { action, x, y } => {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(CONTROL_WRITE_TIMEOUT_SECS),
                    ScrcpyControl::inject_touch(&mut stream, action, x, y, screen_w, screen_h),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("发送触控消息超时".into()),
                }
            },
            ControlMsg::Scroll {
                x,
                y,
                h_scroll,
                v_scroll,
                buttons,
            } => {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(CONTROL_WRITE_TIMEOUT_SECS),
                    ScrcpyControl::inject_scroll(
                        &mut stream,
                        x,
                        y,
                        screen_w,
                        screen_h,
                        h_scroll,
                        v_scroll,
                        buttons,
                    ),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("发送滚轮消息超时".into()),
                }
            },
            ControlMsg::Key { keycode, meta_state } => {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(CONTROL_WRITE_TIMEOUT_SECS),
                    ScrcpyControl::inject_key(&mut stream, keycode, meta_state),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("发送按键消息超时".into()),
                }
            },
            ControlMsg::Text(text) => {
                if text.is_ascii() {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(CONTROL_WRITE_TIMEOUT_SECS),
                        ScrcpyControl::inject_text(&mut stream, &text),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => Err("发送 ASCII 文本超时".into()),
                    }
                } else {
                    let chunks = split_utf8_chunks(&text, NON_ASCII_PASTE_CHUNK_BYTES);
                    let mut paste_result = Ok(());
                    for (index, chunk) in chunks.iter().enumerate() {
                        let sequence = next_clipboard_sequence.fetch_add(1, Ordering::Relaxed);
                        let (ack_tx, ack_rx) = oneshot::channel();
                        pending_acks.lock().await.insert(sequence, ack_tx);
                        remember_local_clipboard(&local_clipboards, chunk).await;

                        let send_result = match tokio::time::timeout(
                            std::time::Duration::from_secs(CONTROL_WRITE_TIMEOUT_SECS),
                            ScrcpyControl::set_clipboard(&mut stream, chunk, true, sequence),
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => Err("发送剪贴板粘贴消息超时".into()),
                        };

                        if let Err(e) = send_result {
                            pending_acks.lock().await.remove(&sequence);
                            paste_result = Err(e);
                            break;
                        }

                        match tokio::time::timeout(
                            std::time::Duration::from_millis(CLIPBOARD_ACK_TIMEOUT_MS),
                            ack_rx,
                        )
                        .await
                        {
                            Ok(Ok(())) => {},
                            Ok(Err(_)) => {
                                paste_result = Err("设备剪贴板确认通道已关闭".into());
                                break;
                            },
                            Err(_) => {
                                pending_acks.lock().await.remove(&sequence);
                                paste_result = Err("等待设备剪贴板确认超时".into());
                                break;
                            },
                        }

                        if index + 1 < chunks.len() {
                            tokio::time::sleep(std::time::Duration::from_millis(
                                NON_ASCII_PASTE_CHUNK_DELAY_MS,
                            ))
                            .await;
                        }
                    }

                    paste_result
                }
            },
            ControlMsg::Back => {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(CONTROL_WRITE_TIMEOUT_SECS),
                    ScrcpyControl::press_back(&mut stream),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("发送返回键消息超时".into()),
                }
            },
            ControlMsg::ResetVideo => {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(CONTROL_WRITE_TIMEOUT_SECS),
                    ScrcpyControl::reset_video(&mut stream),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("发送视频重置消息超时".into()),
                }
            },
        };
        if let Err(e) = result {
            eprintln!("[control-actor] 发送失败: {}", e);
            cancel.cancel();
            break;
        }
    }

    pending_acks.lock().await.clear();
}

async fn control_reader_actor(
    serial: String,
    mut stream: OwnedReadHalf,
    app_handle: tauri::AppHandle,
    pending_acks: ClipboardAckMap,
    local_clipboards: ClipboardEchoGuards,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = ScrcpyControl::read_device_message(&mut stream) => {
                match result {
                    Ok(DeviceMessage::Clipboard(text)) => {
                        if should_suppress_clipboard_echo(&local_clipboards, &text).await {
                            continue;
                        }
                        let payload = ClipboardSyncPayload { serial: serial.clone(), text };
                        let _ = app_handle.emit("scrcpy-clipboard", payload);
                    }
                    Ok(DeviceMessage::AckClipboard(sequence)) => {
                        if let Some(tx) = pending_acks.lock().await.remove(&sequence) {
                            let _ = tx.send(());
                        }
                    }
                    Ok(DeviceMessage::UhidOutput { id, data }) => {
                        eprintln!("[scrcpy] 忽略 UHID 输出: serial={}, id={}, len={}", serial, id, data.len());
                    }
                    Err(e) => {
                        if !cancel.is_cancelled() {
                            eprintln!("[control-reader] 读取失败: {}", e);
                        }
                        cancel.cancel();
                        break;
                    }
                }
            }
        }
    }

    pending_acks.lock().await.clear();
}

fn clipboard_fingerprint(text: &str) -> (u64, usize) {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    (hasher.finish(), text.len())
}

async fn remember_local_clipboard(local_clipboards: &ClipboardEchoGuards, text: &str) {
    let expires_at = Instant::now() + Duration::from_secs(CLIPBOARD_ECHO_GUARD_TTL_SECS);
    let (hash, len) = clipboard_fingerprint(text);
    let mut guards = local_clipboards.lock().await;
    prune_clipboard_guards(&mut guards);
    guards.push_back(ClipboardEchoGuard { hash, len, expires_at });
}

async fn should_suppress_clipboard_echo(
    local_clipboards: &ClipboardEchoGuards,
    text: &str,
) -> bool {
    let (hash, len) = clipboard_fingerprint(text);
    let mut guards = local_clipboards.lock().await;
    prune_clipboard_guards(&mut guards);
    let now = Instant::now();
    if let Some(index) = guards
        .iter()
        .position(|guard| guard.hash == hash && guard.len == len && guard.expires_at > now)
    {
        guards.remove(index);
        return true;
    }
    false
}

fn prune_clipboard_guards(guards: &mut VecDeque<ClipboardEchoGuard>) {
    let now = Instant::now();
    while let Some(front) = guards.front() {
        if front.expires_at > now {
            break;
        }
        guards.pop_front();
    }
}

fn split_utf8_chunks(text: &str, max_bytes: usize) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }

    let bytes = text.as_bytes();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < bytes.len() {
        let remaining = &bytes[start..];
        let chunk_len = super::control::utf8_truncation_index(remaining, max_bytes);
        if chunk_len == 0 {
            break;
        }
        let end = start + chunk_len;
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

fn emit_session_state(
    app_handle: &tauri::AppHandle,
    serial: &str,
    phase: &'static str,
    width: u32,
    height: u32,
    recover_reason: Option<String>,
    last_frame_at_ms: Option<u64>,
) {
    let _ = app_handle.emit(
        constants::tauri_event::SCRCPY_SESSION_STATE,
        ScrcpySessionStatePayload {
            serial: serial.to_string(),
            phase,
            width,
            height,
            recover_reason,
            last_frame_at_ms,
        },
    );
}

// ─── SessionManager ─────────────────────────────────────────

pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, ScrcpySession>>>,
    starting: Arc<Mutex<HashSet<String>>>,
    adb_keyboard_available: Arc<Mutex<HashMap<String, bool>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            starting: Arc::new(Mutex::new(HashSet::new())),
            adb_keyboard_available: Arc::new(Mutex::new(HashMap::new())),
        }
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
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), session.control_handle)
                .await;
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                session.control_reader_handle,
            )
            .await;
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
        {
            let sessions = self.sessions.read().await;
            if sessions.contains_key(serial) {
                return Err(format!("设备 {} 已在投屏中", serial));
            }
        }

        {
            let mut starting = self.starting.lock().await;
            if !starting.insert(serial.to_string()) {
                return Err(format!("设备 {} 正在建立投屏链路", serial));
            }
        }

        let start_result = ScrcpyServer::start(serial, jar_path).await;
        let mut server = match start_result {
            Ok(server) => server,
            Err(e) => {
                self.starting.lock().await.remove(serial);
                return Err(e);
            },
        };

        let screen_w = server.screen_width;
        let screen_h = server.screen_height;
        emit_session_state(&app_handle, serial, "starting", screen_w, screen_h, None, None);

        let video_stream = match server.video_stream.take() {
            Some(stream) => stream,
            None => {
                server.stop().await;
                self.starting.lock().await.remove(serial);
                return Err("video stream 未建立".into());
            },
        };
        let control_stream = match server.control_stream.take() {
            Some(stream) => stream,
            None => {
                server.stop().await;
                self.starting.lock().await.remove(serial);
                return Err("control stream 未建立".into());
            },
        };
        let (control_read, control_write) = control_stream.into_split();

        // P0 优化：启动控制流 Actor（bounded=32，try_send 背压丢帧）
        let (control_tx, control_rx) = tokio::sync::mpsc::channel::<ControlMsg>(32);
        let pending_acks: ClipboardAckMap = Arc::new(Mutex::new(HashMap::new()));
        let local_clipboards: ClipboardEchoGuards = Arc::new(Mutex::new(VecDeque::new()));
        let next_clipboard_sequence = Arc::new(AtomicU64::new(1));
        let cancel = CancellationToken::new();
        let control_handle = tokio::spawn(control_actor(
            control_write,
            control_rx,
            Arc::clone(&pending_acks),
            Arc::clone(&local_clipboards),
            Arc::clone(&next_clipboard_sequence),
            screen_w,
            screen_h,
            cancel.clone(),
        ));
        let control_reader_handle = tokio::spawn(control_reader_actor(
            serial.to_string(),
            control_read,
            app_handle.clone(),
            Arc::clone(&pending_acks),
            Arc::clone(&local_clipboards),
            cancel.clone(),
        ));

        let serial_owned = serial.to_string();
        let cancel_clone = cancel.clone();
        let sessions_ref = Arc::clone(&self.sessions);
        let app_handle_for_pump = app_handle.clone();

        // C-2 修复：保存 JoinHandle
        let pump_handle = tokio::spawn(async move {
            Self::frame_pump(
                serial_owned,
                video_stream,
                server,
                app_handle_for_pump,
                on_frame,
                cancel_clone,
                sessions_ref,
            )
            .await;
        });

        self.sessions.write().await.insert(
            serial.to_string(),
            ScrcpySession {
                cancel: cancel.clone(),
                screen_width: screen_w,
                screen_height: screen_h,
                pump_handle,
                control_tx,
                control_handle,
                control_reader_handle,
            },
        );
        self.starting.lock().await.remove(serial);

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
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), session.control_handle)
                .await;
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                session.control_reader_handle,
            )
            .await;
            eprintln!("[scrcpy] 停止投屏: {}", serial);
        }
        // pump 可能已先于用户操作退出并清理，不报错
        Ok(())
    }

    pub async fn session_dimensions(&self, serial: &str) -> Option<(u32, u32)> {
        let sessions = self.sessions.read().await;
        sessions.get(serial).map(|session| (session.screen_width, session.screen_height))
    }

    /// 注入触控事件（P0 优化：单次 mpsc send，触控用 try_send 背压丢帧）
    pub async fn inject_touch(
        &self,
        serial: &str,
        action: u8,
        x: u32,
        y: u32,
    ) -> Result<(), String> {
        let control_tx = {
            let sessions = self.sessions.read().await;
            let session =
                sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;
            session.control_tx.clone()
        };

        // 高频 mousemove：try_send 满队列时丢弃，避免积压
        control_tx
            .try_send(ControlMsg::Touch { action, x, y })
            .map_err(|e| format!("控制消息发送失败: {}", e))
    }

    pub async fn inject_scroll(
        &self,
        serial: &str,
        x: u32,
        y: u32,
        h_scroll: f32,
        v_scroll: f32,
        buttons: u32,
    ) -> Result<(), String> {
        let control_tx = {
            let sessions = self.sessions.read().await;
            let session =
                sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;
            session.control_tx.clone()
        };

        control_tx
            .try_send(ControlMsg::Scroll { x, y, h_scroll, v_scroll, buttons })
            .map_err(|e| format!("控制消息发送失败: {}", e))
    }

    /// 注入按键事件
    pub async fn inject_key(
        &self,
        serial: &str,
        keycode: u32,
        meta_state: u32,
    ) -> Result<(), String> {
        let control_tx = {
            let sessions = self.sessions.read().await;
            let session =
                sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;
            session.control_tx.clone()
        };

        control_tx
            .send(ControlMsg::Key { keycode, meta_state })
            .await
            .map_err(|e| format!("控制消息发送失败: {}", e))
    }

    async fn detect_adb_keyboard(&self, serial: &str) -> bool {
        if let Some(cached) = self.adb_keyboard_available.lock().await.get(serial).copied() {
            return cached;
        }

        let available = adb::adb_keyboard_available(serial).await.unwrap_or(false);
        self.adb_keyboard_available.lock().await.insert(serial.to_string(), available);
        available
    }

    /// 注入文本（优先独立输入通道，其次回退到 scrcpy）
    pub async fn inject_text(&self, serial: &str, text: &str) -> Result<TextRoute, String> {
        if !text.is_ascii() && self.detect_adb_keyboard(serial).await {
            match adb::adb_keyboard_input_text(serial, text).await {
                Ok(()) => return Ok(TextRoute::AdbImeText),
                Err(err) => {
                    eprintln!(
                        "[scrcpy] ADB IME 输入失败，回退到剪贴板路径: serial={}, error={}",
                        serial, err
                    );
                }
            }
        }

        let control_tx = {
            let sessions = self.sessions.read().await;
            let session =
                sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;
            session.control_tx.clone()
        };

        control_tx
            .send(ControlMsg::Text(text.to_string()))
            .await
            .map_err(|e| format!("控制消息发送失败: {}", e))?;

        Ok(if text.is_ascii() { TextRoute::AsciiDirect } else { TextRoute::ClipboardFallback })
    }

    /// 注入返回键
    pub async fn press_back(&self, serial: &str) -> Result<(), String> {
        let control_tx = {
            let sessions = self.sessions.read().await;
            let session =
                sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;
            session.control_tx.clone()
        };

        control_tx
            .send(ControlMsg::Back)
            .await
            .map_err(|e| format!("控制消息发送失败: {}", e))
    }

    pub async fn reset_video(&self, serial: &str) -> Result<(), String> {
        let control_tx = {
            let sessions = self.sessions.read().await;
            let session =
                sessions.get(serial).ok_or_else(|| format!("设备 {} 未在投屏", serial))?;
            session.control_tx.clone()
        };

        control_tx
            .send(ControlMsg::ResetVideo)
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
        let mut first_frame_emitted = false;
        let mut last_frame_at_ms = None;
        let width = server.screen_width;
        let height = server.screen_height;

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
                            let now_ms = start.elapsed().as_millis() as u64;
                            last_frame_at_ms = Some(now_ms);
                            if !first_frame_emitted {
                                first_frame_emitted = true;
                                tokio::time::sleep(std::time::Duration::from_millis(
                                    FIRST_FRAME_STATE_EMIT_DEBOUNCE_MS,
                                ))
                                .await;
                                emit_session_state(
                                    &app_handle,
                                    &serial,
                                    "streaming",
                                    width,
                                    height,
                                    None,
                                    last_frame_at_ms,
                                );
                            }
                        }
                        Ok(Ok(None)) => {
                            // 空帧，跳过
                            continue;
                        }
                        Ok(Err(e)) => {
                            eprintln!("[scrcpy] 帧读取失败: {} - {}", serial, e);
                            emit_session_state(
                                &app_handle,
                                &serial,
                                "stalled",
                                width,
                                height,
                                Some(format!("frame-read-error: {}", e)),
                                last_frame_at_ms,
                            );
                            break;
                        }
                        Err(_) => {
                            eprintln!("[scrcpy] 帧读取超时 {}s，断开: {}", FRAME_READ_TIMEOUT_SECS, serial);
                            emit_session_state(
                                &app_handle,
                                &serial,
                                "stalled",
                                width,
                                height,
                                Some(format!("frame-timeout-{}s", FRAME_READ_TIMEOUT_SECS)),
                                last_frame_at_ms,
                            );
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
        emit_session_state(
            &app_handle,
            &serial,
            "stopped",
            width,
            height,
            None,
            last_frame_at_ms,
        );
        eprintln!("[scrcpy] 帧推送结束: {}", serial);
    }
}
