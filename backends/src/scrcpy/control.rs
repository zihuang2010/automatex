//! Scrcpy 控制协议实现
//!
//! 通过 TCP 控制 socket 向 scrcpy-server 发送二进制控制消息。
//! 所有数字字段均使用 Big-Endian（网络字节序）。

use byteorder::{BigEndian, ByteOrder, WriteBytesExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// ─── 消息类型常量 ─────────────────────────────────────────────

const MSG_INJECT_KEYCODE: u8 = 0;
const MSG_INJECT_TEXT: u8 = 1;
const MSG_INJECT_TOUCH: u8 = 2;
const MSG_BACK_OR_SCREEN_ON: u8 = 4;
const MSG_SET_CLIPBOARD: u8 = 9;
const MSG_RESET_VIDEO: u8 = 17;
const DEVICE_MSG_CLIPBOARD: u8 = 0;
const DEVICE_MSG_ACK_CLIPBOARD: u8 = 1;
const DEVICE_MSG_UHID_OUTPUT: u8 = 2;

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
/// scrcpy 协议限制 clipboard 消息最大长度
pub const CLIPBOARD_TEXT_MAX_LENGTH: usize = (1 << 18) - 14;

// ─── 异步发送辅助 ─────────────────────────────────────────────

async fn send<W: AsyncWrite + Unpin>(stream: &mut W, buf: &[u8]) -> Result<(), String> {
    stream.write_all(buf).await.map_err(|e| format!("控制消息发送失败: {}", e))
}

pub(crate) fn utf8_truncation_index(raw: &[u8], limit: usize) -> usize {
    if raw.len() <= limit {
        return raw.len();
    }

    let mut end = limit;
    while end > 0 && std::str::from_utf8(&raw[..end]).is_err() {
        end -= 1;
    }
    end
}

// ─── 控制器 ─────────────────────────────────────────────────

pub struct ScrcpyControl;

pub enum DeviceMessage {
    Clipboard(String),
    AckClipboard(u64),
    UhidOutput { id: u16, data: Vec<u8> },
}

impl ScrcpyControl {
    /// 注入触控事件（固定 32 字节消息）
    pub async fn inject_touch<W: AsyncWrite + Unpin>(
        stream: &mut W,
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
    pub async fn tap<W: AsyncWrite + Unpin>(
        stream: &mut W,
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
    pub async fn swipe<W: AsyncWrite + Unpin>(
        stream: &mut W,
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
    pub async fn inject_key<W: AsyncWrite + Unpin>(
        stream: &mut W,
        keycode: u32,
        meta_state: u32,
    ) -> Result<(), String> {
        Self::send_keycode(stream, ACTION_KEY_DOWN, keycode, meta_state).await?;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        Self::send_keycode(stream, ACTION_KEY_UP, keycode, meta_state).await
    }

    /// 注入文本（ASCII 直发）
    ///
    /// 非 ASCII 文本请改走 `set_clipboard(..., paste=true)`，这样更贴近 scrcpy 官方行为。
    pub async fn inject_text<W: AsyncWrite + Unpin>(stream: &mut W, text: &str) -> Result<(), String> {
        let bytes = text.as_bytes();
        if bytes.len() <= INJECT_TEXT_MAX_LENGTH {
            return Self::inject_text_raw(stream, bytes).await;
        }
        // 分片时必须按 UTF-8 边界切割
        let mut start = 0;
        while start < bytes.len() {
            let remaining = &bytes[start..];
            let chunk_len = utf8_truncation_index(remaining, INJECT_TEXT_MAX_LENGTH);
            if chunk_len == 0 {
                return Err("文本分片失败：无法在 UTF-8 边界切割".into());
            }
            Self::inject_text_raw(stream, &remaining[..chunk_len]).await?;
            start += chunk_len;
        }
        Ok(())
    }

    /// 返回键 / 亮屏（固定 2 字节消息）
    pub async fn press_back<W: AsyncWrite + Unpin>(stream: &mut W) -> Result<(), String> {
        send(stream, &[MSG_BACK_OR_SCREEN_ON, ACTION_KEY_DOWN]).await?;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        send(stream, &[MSG_BACK_OR_SCREEN_ON, ACTION_KEY_UP]).await
    }

    pub async fn reset_video<W: AsyncWrite + Unpin>(stream: &mut W) -> Result<(), String> {
        send(stream, &[MSG_RESET_VIDEO]).await
    }

    /// 粘贴文本到设备：设置设备剪贴板，并可请求设备侧执行粘贴。
    pub async fn set_clipboard<W: AsyncWrite + Unpin>(
        stream: &mut W,
        text: &str,
        paste: bool,
        sequence: u64,
    ) -> Result<(), String> {
        let raw = text.as_bytes();
        let len = utf8_truncation_index(raw, CLIPBOARD_TEXT_MAX_LENGTH);
        let mut buf: Vec<u8> = Vec::with_capacity(14 + len);
        buf.push(MSG_SET_CLIPBOARD);
        WriteBytesExt::write_u64::<BigEndian>(&mut buf, sequence).map_err(|e| e.to_string())?;
        buf.push(if paste { 1 } else { 0 });
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, len as u32).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut buf, &raw[..len]).map_err(|e| e.to_string())?;
        send(stream, &buf).await
    }

    pub async fn read_device_message<R: AsyncRead + Unpin>(
        stream: &mut R,
    ) -> Result<DeviceMessage, String> {
        let msg_type = stream.read_u8().await.map_err(|e| format!("读取设备消息类型失败: {}", e))?;
        match msg_type {
            DEVICE_MSG_CLIPBOARD => {
                let len = stream
                    .read_u32()
                    .await
                    .map_err(|e| format!("读取设备剪贴板长度失败: {}", e))? as usize;
                let mut buf = vec![0u8; len];
                stream
                    .read_exact(&mut buf)
                    .await
                    .map_err(|e| format!("读取设备剪贴板内容失败: {}", e))?;
                let text = String::from_utf8(buf).map_err(|e| format!("设备剪贴板内容不是合法 UTF-8: {}", e))?;
                Ok(DeviceMessage::Clipboard(text))
            },
            DEVICE_MSG_ACK_CLIPBOARD => {
                let sequence = stream
                    .read_u64()
                    .await
                    .map_err(|e| format!("读取剪贴板 ACK 失败: {}", e))?;
                Ok(DeviceMessage::AckClipboard(sequence))
            },
            DEVICE_MSG_UHID_OUTPUT => {
                let id = stream
                    .read_u16()
                    .await
                    .map_err(|e| format!("读取 UHID id 失败: {}", e))?;
                let len = stream
                    .read_u16()
                    .await
                    .map_err(|e| format!("读取 UHID 数据长度失败: {}", e))? as usize;
                let mut data = vec![0u8; len];
                stream
                    .read_exact(&mut data)
                    .await
                    .map_err(|e| format!("读取 UHID 数据失败: {}", e))?;
                Ok(DeviceMessage::UhidOutput { id, data })
            },
            other => Err(format!("未知设备消息类型: {}", other)),
        }
    }

    // ── 内部辅助 ──

    /// 发送单个按键事件（固定 14 字节）
    async fn send_keycode<W: AsyncWrite + Unpin>(
        stream: &mut W,
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
    async fn inject_text_raw<W: AsyncWrite + Unpin>(
        stream: &mut W,
        text_bytes: &[u8],
    ) -> Result<(), String> {
        let mut buf: Vec<u8> = Vec::with_capacity(5 + text_bytes.len());
        buf.push(MSG_INJECT_TEXT);
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, text_bytes.len() as u32)
            .map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut buf, text_bytes).map_err(|e| e.to_string())?;
        send(stream, &buf).await
    }
}
