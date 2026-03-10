//! H.264 视频流读取
//!
//! 从 scrcpy 视频 socket 中读取 H.264 帧数据。

use byteorder::{BigEndian, ByteOrder};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

// Q-3: 协议常量
const DEVICE_NAME_LEN: usize = 64;
const VIDEO_HEADER_LEN: usize = 13;
const FRAME_HEADER_LEN: usize = 12;
const MAX_FRAME_SIZE: usize = 8 * 1024 * 1024; // 8MB

/// 视频帧
pub struct VideoFrame {
    /// 是否为配置帧（SPS/PPS）
    pub is_config: bool,
    /// H.264 NAL unit 数据
    pub data: Vec<u8>,
}

/// 读取 64 字节设备名（scrcpy 连接握手的第一步）
pub async fn read_device_name(stream: &mut TcpStream) -> Result<String, String> {
    let mut name_buf = [0u8; DEVICE_NAME_LEN];
    stream
        .read_exact(&mut name_buf)
        .await
        .map_err(|e| format!("读取设备名失败: {}", e))?;

    let name = String::from_utf8_lossy(&name_buf).trim_end_matches('\0').to_string();
    eprintln!("[scrcpy] 设备名: {}", name);
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

    eprintln!("[scrcpy] 视频 header: codec={}, {}x{}", codec, width, height);
    Ok(VideoHeader { codec, width, height })
}

/// 读取一帧视频数据
///
/// 每帧格式: [PTS: 8B] [size: 4B] [data: size B]
/// PTS 的 bit 63 为 config 标志位
pub async fn read_frame(stream: &mut TcpStream) -> Result<VideoFrame, String> {
    // 读 12 字节 header
    let mut header = [0u8; FRAME_HEADER_LEN];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| format!("读取帧 header 失败: {}", e))?;

    let pts_raw = BigEndian::read_u64(&header[0..8]);
    let is_config = (pts_raw >> 63) & 1 == 1;
    let size = BigEndian::read_u32(&header[8..12]) as usize;

    if size == 0 {
        return Ok(VideoFrame { is_config, data: Vec::new() });
    }

    // 安全限制：单帧不应超过 8MB
    if size > MAX_FRAME_SIZE {
        return Err(format!("帧数据过大: {} bytes", size));
    }

    let mut data = vec![0u8; size];
    stream
        .read_exact(&mut data)
        .await
        .map_err(|e| format!("读取帧数据失败: {}", e))?;

    Ok(VideoFrame { is_config, data })
}
