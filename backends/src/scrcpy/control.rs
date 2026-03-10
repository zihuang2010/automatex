//! Scrcpy 控制协议实现
//!
//! 通过 TCP 控制 socket 向 scrcpy-server 发送二进制控制消息。
//! 所有数字字段均使用 Big-Endian（网络字节序）。

use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use std::io::Write;
use tokio::net::TcpStream;

// ─── 消息类型常量 ─────────────────────────────────────────────

const MSG_INJECT_KEYCODE: u8 = 0;
const MSG_INJECT_TEXT: u8 = 1;
const MSG_INJECT_TOUCH: u8 = 2;
const MSG_BACK_OR_SCREEN_ON: u8 = 4;
const MSG_SET_CLIPBOARD: u8 = 9;

// 触控动作
const ACTION_DOWN: u8 = 0;
const ACTION_UP: u8 = 1;
const ACTION_MOVE: u8 = 2;

// 按键动作
const ACTION_KEY_DOWN: u8 = 0;
const ACTION_KEY_UP: u8 = 1;

// 常量
const POINTER_ID_MOUSE: i64 = -1;
const PRESSURE_FULL: u16 = 0xFFFF;
const PRESSURE_NONE: u16 = 0;

/// scrcpy 协议限制 inject_text 最大长度
const INJECT_TEXT_MAX_LENGTH: usize = 300;

/// Ctrl+V 模拟粘贴用
const KEYCODE_V: u32 = 50;
const META_CTRL_ON: u32 = 0x1000;

// ─── 异步发送辅助 ─────────────────────────────────────────────

async fn send(stream: &mut TcpStream, buf: &[u8]) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    stream.write_all(buf).await.map_err(|e| format!("控制消息发送失败: {}", e))
}

// ─── 控制器 ─────────────────────────────────────────────────

pub struct ScrcpyControl;

impl ScrcpyControl {
    /// 注入触控事件（固定 32 字节消息）
    pub async fn inject_touch(
        stream: &mut TcpStream,
        action: u8,
        x: u32,
        y: u32,
        screen_w: u32,
        screen_h: u32,
    ) -> Result<(), String> {
        // 坐标钳制到屏幕范围内
        let x = x.min(screen_w);
        let y = y.min(screen_h);
        let pressure = if action == ACTION_UP { PRESSURE_NONE } else { PRESSURE_FULL };

        let mut buf = [0u8; 32];
        buf[0] = MSG_INJECT_TOUCH;
        buf[1] = action;
        byteorder::BigEndian::write_i64(&mut buf[2..10], POINTER_ID_MOUSE);
        byteorder::BigEndian::write_u32(&mut buf[10..14], x);
        byteorder::BigEndian::write_u32(&mut buf[14..18], y);
        byteorder::BigEndian::write_u16(&mut buf[18..20], screen_w as u16);
        byteorder::BigEndian::write_u16(&mut buf[20..22], screen_h as u16);
        byteorder::BigEndian::write_u16(&mut buf[22..24], pressure);
        // buf[24..28] = actionButton (0)
        // buf[28..32] = buttons (0)
        send(stream, &buf).await
    }

    /// 点按
    #[allow(dead_code)]
    pub async fn tap(
        stream: &mut TcpStream,
        x: u32,
        y: u32,
        screen_w: u32,
        screen_h: u32,
    ) -> Result<(), String> {
        Self::inject_touch(stream, ACTION_DOWN, x, y, screen_w, screen_h).await?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Self::inject_touch(stream, ACTION_UP, x, y, screen_w, screen_h).await
    }

    /// 滑动
    #[allow(dead_code)]
    pub async fn swipe(
        stream: &mut TcpStream,
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        duration_ms: u64,
        screen_w: u32,
        screen_h: u32,
    ) -> Result<(), String> {
        let steps = 20u32;
        let step_delay = (duration_ms / steps as u64).max(5);

        Self::inject_touch(stream, ACTION_DOWN, x1, y1, screen_w, screen_h).await?;

        for i in 1..=steps {
            let ratio = i as f64 / steps as f64;
            let cx = x1 as f64 + (x2 as f64 - x1 as f64) * ratio;
            let cy = y1 as f64 + (y2 as f64 - y1 as f64) * ratio;
            tokio::time::sleep(std::time::Duration::from_millis(step_delay)).await;
            Self::inject_touch(stream, ACTION_MOVE, cx as u32, cy as u32, screen_w, screen_h)
                .await?;
        }

        Self::inject_touch(stream, ACTION_UP, x2, y2, screen_w, screen_h).await
    }

    /// 注入按键（固定 14 字节消息，KEY_DOWN + KEY_UP）
    pub async fn inject_key(
        stream: &mut TcpStream,
        keycode: u32,
        meta_state: u32,
    ) -> Result<(), String> {
        Self::send_keycode(stream, ACTION_KEY_DOWN, keycode, meta_state).await?;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        Self::send_keycode(stream, ACTION_KEY_UP, keycode, meta_state).await
    }

    /// 注入文本（智能路由：ASCII 走 MSG_INJECT_TEXT，中文等非 ASCII 走剪贴板+Ctrl+V）
    ///
    /// scrcpy 的 MSG_INJECT_TEXT 内部使用 KeyCharacterMap.getEvents()，
    /// 该 API 不支持 CJK 字符。非 ASCII 文本通过 SET_CLIPBOARD(paste=false)
    /// 设置剪贴板内容，再通过 inject_key(KEYCODE_V + CTRL) 触发应用侧粘贴。
    pub async fn inject_text(stream: &mut TcpStream, text: &str) -> Result<(), String> {
        if text.is_ascii() {
            // ASCII：直接用 MSG_INJECT_TEXT（快速，无剪贴板副作用）
            let bytes = text.as_bytes();
            if bytes.len() <= INJECT_TEXT_MAX_LENGTH {
                return Self::inject_text_raw(stream, bytes).await;
            }
            // 分片
            let mut start = 0;
            while start < text.len() {
                let end = (start + INJECT_TEXT_MAX_LENGTH).min(text.len());
                Self::inject_text_raw(stream, text[start..end].as_bytes()).await?;
                start = end;
            }
            Ok(())
        } else {
            // 非 ASCII（中文等）：SET_CLIPBOARD(paste=false) + Ctrl+V
            Self::set_clipboard(stream, text).await?;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Self::inject_key(stream, KEYCODE_V, META_CTRL_ON).await
        }
    }

    /// 返回键 / 亮屏（固定 2 字节消息）
    pub async fn press_back(stream: &mut TcpStream) -> Result<(), String> {
        send(stream, &[MSG_BACK_OR_SCREEN_ON, ACTION_KEY_DOWN]).await?;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        send(stream, &[MSG_BACK_OR_SCREEN_ON, ACTION_KEY_UP]).await
    }

    // ── 内部辅助 ──

    /// 发送单个按键事件（固定 14 字节）
    async fn send_keycode(
        stream: &mut TcpStream,
        action: u8,
        keycode: u32,
        meta_state: u32,
    ) -> Result<(), String> {
        let mut buf = [0u8; 14];
        buf[0] = MSG_INJECT_KEYCODE;
        buf[1] = action;
        byteorder::BigEndian::write_u32(&mut buf[2..6], keycode);
        // buf[6..10] = repeat (0)
        byteorder::BigEndian::write_u32(&mut buf[10..14], meta_state);
        send(stream, &buf).await
    }

    /// 发送原始文本 inject 消息
    async fn inject_text_raw(stream: &mut TcpStream, text_bytes: &[u8]) -> Result<(), String> {
        let mut buf: Vec<u8> = Vec::with_capacity(5 + text_bytes.len());
        buf.push(MSG_INJECT_TEXT);
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, text_bytes.len() as u32)
            .map_err(|e| e.to_string())?;
        buf.write_all(text_bytes).map_err(|e| e.to_string())?;
        send(stream, &buf).await
    }

    /// 设置 Android 剪贴板内容（paste=false，仅设置不粘贴，不阻塞）
    /// 格式: type(1) + sequence(8) + paste(1) + text_len(4) + text(N)
    async fn set_clipboard(stream: &mut TcpStream, text: &str) -> Result<(), String> {
        let text_bytes = text.as_bytes();
        let mut buf: Vec<u8> = Vec::with_capacity(14 + text_bytes.len());
        buf.push(MSG_SET_CLIPBOARD);
        // sequence: 8 bytes (u64 = 0)
        WriteBytesExt::write_u64::<BigEndian>(&mut buf, 0).map_err(|e| e.to_string())?;
        // paste: false (0) — 仅设置剪贴板，不触发 commitText
        buf.push(0);
        // text length
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, text_bytes.len() as u32)
            .map_err(|e| e.to_string())?;
        buf.write_all(text_bytes).map_err(|e| e.to_string())?;
        send(stream, &buf).await
    }
}
