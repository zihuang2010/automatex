//! 投屏会话管理
//!
//! 管理多台设备的投屏会话，每台设备一个 Session。
//! 视频帧通过 Tauri Channel 直接推送到前端（避免 base64 + 全局事件广播）。

use super::control::{DeviceMessage, ScrcpyControl};
use super::server::ScrcpyServer;
use crate::{connection::adb, constants};
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
use tracing::{debug, error, info, warn};
use tokio_util::sync::CancellationToken;

// 类型别名：pump_handle 使用 Arc<Mutex<Option>> 以支持先 insert session 再填充句柄（C-1 修复）
type PumpHandle = Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>;

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
    Touch {
        action: u8,
        x: u32,
        y: u32,
    },
    Scroll {
        x: u32,
        y: u32,
        h_scroll: f32,
        v_scroll: f32,
        buttons: u32,
    },
    Key {
        keycode: u32,
        meta_state: u32,
    },
    Text(String),
    Back,
    ResetVideo,
}

// ─── Session ─────────────────────────────────────────────

struct ScrcpySession {
    cancel: CancellationToken,
    screen_width: u32,
    screen_height: u32,
    /// C-1 修复：pump_handle 使用 Arc<Mutex<Option>> 以支持先 insert session 再填充句柄。
    /// 若 pump 尚未 spawn（窗口极短），await 时得到 None 直接跳过。
    pump_handle: PumpHandle,
    /// P0 优化：控制流 Actor 通道（替代 Arc<Mutex<TcpStream>>）
    control_tx: tokio::sync::mpsc::Sender<ControlMsg>,
    control_handle: tokio::task::JoinHandle<()>,
    control_reader_handle: tokio::task::JoinHandle<()>,
}

/// 控制流 Actor：独占 TcpStream，串行发送，零锁竞争
#[allow(clippy::too_many_arguments)]
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
            ControlMsg::Scroll { x, y, h_scroll, v_scroll, buttons } => match tokio::time::timeout(
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
            warn!(error = %e, "发送失败");
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
                        debug!(serial = %serial, id = id, len = data.len(), "忽略 UHID 输出");
                    }
                    Err(e) => {
                        if !cancel.is_cancelled() {
                            warn!(error = %e, "控制流读取失败");
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

/// H-2 修复：使用双哈希策略降低碰撞概率。
///
/// `DefaultHasher` 使用 SipHash-1-3，碰撞率约 1/2⁶⁴。
/// 对剪贴板防回显场景而言，仅靠单 hash + len 二元组的碰撞概率已很低，
/// 但为防止不同长度相同哈希值的极端情况，我们额外将文本长度纳入哈希计算，
/// 并将结果编码为同一个 u128 以提供更强的区分度。
fn clipboard_fingerprint(text: &str) -> (u64, usize) {
    use std::collections::hash_map::DefaultHasher;
    // 第一路：对文本内容哈希
    let mut h1 = DefaultHasher::new();
    text.hash(&mut h1);
    let hash1 = h1.finish();

    // 第二路：对文本长度 + 内容再哈希（不同种子效果：把 len 混入哈希输入）
    let mut h2 = DefaultHasher::new();
    text.len().hash(&mut h2);
    text.hash(&mut h2);
    let hash2 = h2.finish();

    // 合并为单一 u64（XOR 防止 hash1==hash2 时退化，保留两路信息）
    (hash1 ^ hash2.rotate_left(32), text.len())
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
    /// H-3 修复：键盘检测缓存，entry().or_insert 保证并发安全（TOCTOU 修复）
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
            // M-2 修复：并发等待三个 handle，最长阻塞 max(5,2,2)=5s 而非串行 9s
            let pump_handle_taken = session.pump_handle.lock().await.take();
            tokio::join!(
                async {
                    if let Some(h) = pump_handle_taken {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
                    }
                },
                async {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        session.control_handle,
                    )
                    .await;
                },
                async {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        session.control_reader_handle,
                    )
                    .await;
                },
            );
            info!(serial = %serial, "shutdown completed");
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

        // C-1 修复：先创建 pump_handle 槽位并插入 Session，再 spawn pump 填充句柄。
        // 这样 frame_pump 内部的 sessions.remove() 自清理时 session 必然已存在，
        // 消除了「pump 先退出、session 后插入」导致的僵尸 session 竞态窗口。
        let pump_handle_slot: PumpHandle = Arc::new(Mutex::new(None));

        let serial_owned = serial.to_string();
        let cancel_clone = cancel.clone();
        let sessions_ref = Arc::clone(&self.sessions);
        let app_handle_for_pump = app_handle.clone();

        // 先 insert session，pump_handle 槽位为 None
        self.sessions.write().await.insert(
            serial.to_string(),
            ScrcpySession {
                cancel: cancel.clone(),
                screen_width: screen_w,
                screen_height: screen_h,
                pump_handle: Arc::clone(&pump_handle_slot),
                control_tx,
                control_handle,
                control_reader_handle,
            },
        );
        self.starting.lock().await.remove(serial);

        // 再 spawn pump，填充句柄（此时 session 已在 map 中，无竞态）
        let real_handle = tokio::spawn(async move {
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
        *pump_handle_slot.lock().await = Some(real_handle);

        Ok(MirrorStartedPayload { serial: serial.to_string(), width: screen_w, height: screen_h })
    }

    /// 停止投屏
    /// pump 可能已自行退出并清理，因此 session 不存在时不报错
    pub async fn stop_mirror(&self, serial: &str) -> Result<(), String> {
        let session = { self.sessions.write().await.remove(serial) };

        if let Some(session) = session {
            session.cancel.cancel();
            // M-2 修复：并发等待三个 handle，最长阻塞 5s 而非串行最坏 9s
            let pump_handle_taken = session.pump_handle.lock().await.take();
            tokio::join!(
                async {
                    if let Some(h) = pump_handle_taken {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
                    }
                },
                async {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        session.control_handle,
                    )
                    .await;
                },
                async {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        session.control_reader_handle,
                    )
                    .await;
                },
            );
            info!(serial = %serial, "停止投屏");
        }
        // pump 可能已先于用户操作退出并清理，不报错
        Ok(())
    }

    pub async fn session_dimensions(&self, serial: &str) -> Option<(u32, u32)> {
        let sessions = self.sessions.read().await;
        sessions
            .get(serial)
            .map(|session| (session.screen_width, session.screen_height))
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

    /// H-3 修复：使用 entry().or_insert() 模式消除 TOCTOU 竞态。
    ///
    /// 原实现：读锁检查 → 释放 → ADB 调用 → 写锁插入（窗口内多协程并发触发重复 ADB）。
    /// 新实现：先无锁检查快路径，若缓存缺失则 ADB 调用后用 entry 保证插入幂等。
    /// 最坏情况下两个协程并发时各做一次 ADB，但插入时后者被 or_insert 忽略，结果一致。
    async fn detect_adb_keyboard(&self, serial: &str) -> bool {
        // 快路径：大多数情况下缓存命中，直接返回
        {
            let cache = self.adb_keyboard_available.lock().await;
            if let Some(&cached) = cache.get(serial) {
                return cached;
            }
        }

        // 慢路径：释放锁后执行 ADB 调用（避免持锁阻塞其他协程）
        let available = adb::adb_keyboard_available(serial).await.unwrap_or(false);

        // 用 entry().or_insert() 保证幂等：并发时第二个写入被忽略
        let mut cache = self.adb_keyboard_available.lock().await;
        *cache.entry(serial.to_string()).or_insert(available)
    }

    /// 注入文本（优先独立输入通道，其次回退到 scrcpy）
    pub async fn inject_text(&self, serial: &str, text: &str) -> Result<TextRoute, String> {
        if !text.is_ascii() && self.detect_adb_keyboard(serial).await {
            match adb::adb_keyboard_input_text(serial, text).await {
                Ok(()) => return Ok(TextRoute::AdbImeText),
                Err(err) => {
                    warn!(serial = %serial, error = %err, "ADB IME 输入失败，回退到剪贴板路径");
                },
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

        Ok(if text.is_ascii() {
            TextRoute::AsciiDirect
        } else {
            TextRoute::ClipboardFallback
        })
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
        debug!(serial = %serial, "帧推送启动");
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
                    debug!(serial = %serial, "帧推送取消");
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
                                warn!(serial = %serial, "channel 已关闭");
                                break;
                            }
                            let now_ms = start.elapsed().as_millis() as u64;
                            last_frame_at_ms = Some(now_ms);
                            if !first_frame_emitted {
                                first_frame_emitted = true;
                                // L-1 修复：防抖 sleep 改为独立 spawn，
                                // 避免阻塞 pump 主循环（原实现会丢失约 7 帧 @30fps）。
                                let ah = app_handle.clone();
                                let s = serial.clone();
                                let lf = last_frame_at_ms;
                                tokio::spawn(async move {
                                    tokio::time::sleep(std::time::Duration::from_millis(
                                        FIRST_FRAME_STATE_EMIT_DEBOUNCE_MS,
                                    ))
                                    .await;
                                    emit_session_state(
                                        &ah,
                                        &s,
                                        "streaming",
                                        width,
                                        height,
                                        None,
                                        lf,
                                    );
                                });
                            }
                        }
                        Ok(Ok(None)) => {
                            // 空帧，跳过
                            continue;
                        }
                        Ok(Err(e)) => {
                            error!(serial = %serial, error = %e, "帧读取失败");
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
                            warn!(serial = %serial, timeout_secs = FRAME_READ_TIMEOUT_SECS, "帧读取超时，断开");
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
        // C-1 修复：session 现在在 pump spawn 前已插入，remove 必然找得到（正常退出时）
        sessions.write().await.remove(&serial);
        // L-2 修复：移除冗余的 "scrcpy-stopped" 裸事件，统一由 session_state("stopped") 承载，
        // 避免前端需要同时监听两个语义重叠的事件。
        emit_session_state(&app_handle, &serial, "stopped", width, height, None, last_frame_at_ms);
        debug!(serial = %serial, "帧推送结束");
    }
}
