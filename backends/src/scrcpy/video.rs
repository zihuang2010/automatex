//! H.264 视频流读取
//!
//! 从 scrcpy 视频 socket 中读取 H.264 帧数据。

use byteorder::{BigEndian, ByteOrder};
use tokio::io::AsyncReadExt;
use tracing::debug;
use tokio::net::TcpStream;

// Q-3: 协议常量
const DEVICE_NAME_LEN: usize = 64;
const VIDEO_HEADER_LEN: usize = 13;
const FRAME_HEADER_LEN: usize = 12;
const MAX_FRAME_SIZE: usize = 8 * 1024 * 1024; // 8MB

/// P1 优化：帧缓冲池 — 预分配 Vec，复用内存避免每帧 alloc
///
/// **内存收缩策略（H-1 修复）**：若 buf 容量超过 `POOL_SHRINK_HIGH_WATER`（2MB）
/// 且当前帧远小于该阈值，主动收缩至 `POOL_INITIAL_CAPACITY`（512KB），
/// 防止单次异常大帧（≤8MB）导致 buf 永久持有高水位容量。
pub struct FramePool {
    buf: Vec<u8>,
}

/// 高水位：超过此容量且帧小时触发收缩
const POOL_SHRINK_HIGH_WATER: usize = 2 * 1024 * 1024; // 2 MB
/// 收缩目标：回退到初始预分配大小
const POOL_INITIAL_CAPACITY: usize = 512 * 1024; // 512 KB
/// 收缩触发阈值：当前帧小于高水位的 1/4 时才收缩，避免抖动
const POOL_SHRINK_FRAME_THRESHOLD: usize = POOL_SHRINK_HIGH_WATER / 4; // 512 KB

impl FramePool {
    /// 预分配 512KB（可容纳大多数 I-frame）
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(POOL_INITIAL_CAPACITY) }
    }

    /// P2 优化：读取帧并直接编码为传输格式（仅一次拷贝）
    ///
    /// 输出格式: [is_config: 1B][ts: 8B big-endian][data: NB]
    /// 相比 read_frame + encode_frame_binary 的两次拷贝，此方法仅拷贝一次。
    pub async fn read_frame_encoded(
        &mut self,
        stream: &mut TcpStream,
        ts: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        let mut header = [0u8; FRAME_HEADER_LEN];
        stream
            .read_exact(&mut header)
            .await
            .map_err(|e| format!("读取帧 header 失败: {}", e))?;

        let pts_raw = BigEndian::read_u64(&header[0..8]);
        let is_config = (pts_raw >> 63) & 1 == 1;
        let size = BigEndian::read_u32(&header[8..12]) as usize;

        if size == 0 {
            return Ok(None);
        }
        if size > MAX_FRAME_SIZE {
            return Err(format!("帧数据过大: {} bytes", size));
        }

        let total = 9 + size;

        // H-1 修复：高水位内存收缩，防止异常大帧导致 buf 永久膨胀
        if self.buf.capacity() > POOL_SHRINK_HIGH_WATER && total < POOL_SHRINK_FRAME_THRESHOLD {
            self.buf = Vec::with_capacity(POOL_INITIAL_CAPACITY);
        }

        // 直接在 buf 中组装完整输出：[header 9B] + [data NB]
        self.buf.resize(total, 0);
        self.buf[0] = if is_config { 1 } else { 0 };
        self.buf[1..9].copy_from_slice(&ts.to_be_bytes());
        stream
            .read_exact(&mut self.buf[9..total])
            .await
            .map_err(|e| format!("读取帧数据失败: {}", e))?;

        // 仅一次拷贝（buf 保留容量供下次复用）
        Ok(Some(self.buf[..total].to_vec()))
    }
}

/// 读取 64 字节设备名（scrcpy 连接握手的第一步）
pub async fn read_device_name(stream: &mut TcpStream) -> Result<String, String> {
    let mut name_buf = [0u8; DEVICE_NAME_LEN];
    stream
        .read_exact(&mut name_buf)
        .await
        .map_err(|e| format!("读取设备名失败: {}", e))?;

    let name = String::from_utf8_lossy(&name_buf).trim_end_matches('\0').to_string();
    debug!(device_name = %name, "设备名");
    Ok(name)
}

/// 视频流 header 信息（scrcpy v3.x）
pub struct VideoHeader {
    #[allow(dead_code)]
    pub codec: String,
    pub width: u32,
    pub height: u32,
}

/// 读取 scrcpy v3.x 的 13 字节视频 header
///
/// 格式: [0x00] [codec: 4B] [width: 4B] [height: 4B]
pub async fn read_video_header(stream: &mut TcpStream) -> Result<VideoHeader, String> {
    let mut header = [0u8; VIDEO_HEADER_LEN];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| format!("读取视频 header 失败: {}", e))?;

    // 跳过第 1 个 dummy 字节
    let codec_bytes = &header[1..5];
    let codec = String::from_utf8_lossy(codec_bytes).to_string();
    let width = BigEndian::read_u32(&header[5..9]);
    let height = BigEndian::read_u32(&header[9..13]);

    debug!(codec = %codec, width = width, height = height, "视频 header");
    Ok(VideoHeader { codec, width, height })
}
