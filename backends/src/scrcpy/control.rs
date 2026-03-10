//! Scrcpy 控制协议实现
//!
//! 通过 TCP 控制 socket 向 scrcpy-server 发送二进制控制消息。
//! 所有数字字段均使用 Big-Endian（网络字节序）。

use byteorder::{BigEndian, WriteBytesExt};
use std::io::Write;
use tokio::net::TcpStream;

// ─── 消息类型常量 ─────────────────────────────────────────────

const MSG_INJECT_KEYCODE: u8 = 0;
const MSG_INJECT_TEXT: u8 = 1;
const MSG_INJECT_TOUCH: u8 = 2;
#[allow(dead_code)]
const MSG_INJECT_SCROLL: u8 = 3;
const MSG_BACK_OR_SCREEN_ON: u8 = 4;

// 触控动作
#[allow(dead_code)]
const ACTION_DOWN: u8 = 0;
const ACTION_UP: u8 = 1;
#[allow(dead_code)]
const ACTION_MOVE: u8 = 2;

// 按键动作
const ACTION_KEY_DOWN: u8 = 0;
const ACTION_KEY_UP: u8 = 1;

// 常量
const POINTER_ID_MOUSE: i64 = -1;
const PRESSURE_FULL: u16 = 0xFFFF;
const PRESSURE_NONE: u16 = 0;

// ─── 异步发送辅助 ─────────────────────────────────────────────

async fn send(stream: &mut TcpStream, buf: &[u8]) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    stream.write_all(buf).await.map_err(|e| format!("控制消息发送失败: {}", e))
}

// ─── 控制器 ─────────────────────────────────────────────────

pub struct ScrcpyControl;

impl ScrcpyControl {
    /// 注入触控事件
    /// SG-2 修复：接受 u32 屏幕尺寸，内部截断为 u16（scrcpy 协议要求）
    pub async fn inject_touch(
        stream: &mut TcpStream,
        action: u8,
        x: u32,
        y: u32,
        screen_w: u32,
        screen_h: u32,
    ) -> Result<(), String> {
        // S-3: 坐标钳制到屏幕范围内
        let x = x.min(screen_w);
        let y = y.min(screen_h);
        let pressure = if action == ACTION_UP { PRESSURE_NONE } else { PRESSURE_FULL };

        let mut buf: Vec<u8> = Vec::with_capacity(32);
        buf.push(MSG_INJECT_TOUCH);
        buf.push(action);
        WriteBytesExt::write_i64::<BigEndian>(&mut buf, POINTER_ID_MOUSE)
            .map_err(|e| e.to_string())?;
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, x).map_err(|e| e.to_string())?;
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, y).map_err(|e| e.to_string())?;
        // scrcpy 协议中屏幕尺寸字段为 u16
        WriteBytesExt::write_u16::<BigEndian>(&mut buf, screen_w as u16)
            .map_err(|e| e.to_string())?;
        WriteBytesExt::write_u16::<BigEndian>(&mut buf, screen_h as u16)
            .map_err(|e| e.to_string())?;
        WriteBytesExt::write_u16::<BigEndian>(&mut buf, pressure).map_err(|e| e.to_string())?;
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, 0).map_err(|e| e.to_string())?; // actionButton
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, 0).map_err(|e| e.to_string())?; // buttons

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
        let step_delay = duration_ms / steps as u64;

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

    /// 注入按键
    pub async fn inject_key(
        stream: &mut TcpStream,
        keycode: u32,
        meta_state: u32,
    ) -> Result<(), String> {
        Self::send_keycode(stream, ACTION_KEY_DOWN, keycode, meta_state).await?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Self::send_keycode(stream, ACTION_KEY_UP, keycode, meta_state).await
    }

    /// 注入文本
    #[allow(dead_code)]
    pub async fn inject_text(stream: &mut TcpStream, text: &str) -> Result<(), String> {
        let text_bytes = text.as_bytes();
        let mut buf: Vec<u8> = Vec::with_capacity(5 + text_bytes.len());
        buf.push(MSG_INJECT_TEXT);
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, text_bytes.len() as u32)
            .map_err(|e| e.to_string())?;
        buf.write_all(text_bytes).map_err(|e| e.to_string())?;
        send(stream, &buf).await
    }

    /// 返回键 / 亮屏
    pub async fn press_back(stream: &mut TcpStream) -> Result<(), String> {
        let buf = [MSG_BACK_OR_SCREEN_ON, ACTION_KEY_DOWN];
        send(stream, &buf).await?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let buf = [MSG_BACK_OR_SCREEN_ON, ACTION_KEY_UP];
        send(stream, &buf).await
    }

    // ── 内部辅助 ──

    async fn send_keycode(
        stream: &mut TcpStream,
        action: u8,
        keycode: u32,
        meta_state: u32,
    ) -> Result<(), String> {
        let mut buf: Vec<u8> = Vec::with_capacity(14);
        buf.push(MSG_INJECT_KEYCODE);
        buf.push(action);
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, keycode).map_err(|e| e.to_string())?;
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, 0).map_err(|e| e.to_string())?; // repeat
        WriteBytesExt::write_u32::<BigEndian>(&mut buf, meta_state).map_err(|e| e.to_string())?;
        send(stream, &buf).await
    }
}
