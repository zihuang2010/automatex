//! 手机无障碍 App TCP 客户端
//!
//! ## 协议（NDJSON over raw TCP）
//!
//! **PC → 手机**（单行 JSON + `\n`）：
//! ```json
//! {
//!   "type": "batch",
//!   "batch_id": "job-001",
//!   "tasks": [
//!     { "city": "上海站", "keywords": ["安装灯具", "维修灯具"] }
//!   ],
//!   "max_pages": 3
//! }
//! ```
//!
//! **手机 → PC**（NDJSON 流，每行一个 JSON 对象）：
//! ```json
//! { "type": "result", "batch_id": "job-001", "city": "上海站", "keyword": "安装灯具", "items": [...] }
//! { "type": "done",   "batch_id": "job-001", "total_items": 12, "completed_tasks": 3, "total_tasks": 4, "stopped": false, "fatal_reason": null }
//! ```
//!
//! ## 使用方式
//!
//! ```rust,ignore
//! let client = PhoneClient::new(local_port);
//! let mut stream = client.open(batch_id, &tasks, max_pages).await?;
//! while let Some(msg) = stream.next_msg().await? {
//!     match msg {
//!         PhoneMsg::Result { city, keyword, items } => { /* 处理结果 */ }
//!         PhoneMsg::Done(info) => { /* 批次完成 */ break; }
//!     }
//! }
//! ```

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use tracing::{debug, warn};

use crate::constants::phone_client as cfg;

// ─── 请求结构 ─────────────────────────────────────────────────────────────────

/// 批次任务项：一个城市 + POI + 多个待扫描关键词
#[derive(Debug, Clone, Serialize)]
pub struct BatchTask {
    pub city: String,
    pub poi: String,
    pub keywords: Vec<String>,
}

/// 批次请求（序列化后作为一行 JSON 发送）
#[derive(Serialize)]
struct BatchRequest<'a> {
    r#type: &'static str,
    batch_id: &'a str,
    tasks: &'a [BatchTask],
    max_pages: u32,
}

// ─── 响应结构 ─────────────────────────────────────────────────────────────────

/// 单个采集结果条目
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResultItem {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub captured_at: String,
}

/// 批次完成摘要（来自 `"type":"done"` 消息）
#[derive(Debug)]
pub struct BatchDoneInfo {
    pub total_items: u32,
    pub completed_tasks: u32,
    pub total_tasks: u32,
    /// 手机端是否提前停止（用户取消 / 超时 / 风控等）
    pub stopped: bool,
    /// 严重错误原因（非 None 时调用方应考虑标记设备）
    pub fatal_reason: Option<String>,
}

/// 已解析的手机端消息（业务层使用）
pub enum PhoneMsg {
    Result {
        city: String,
        keyword: String,
        items: Vec<ResultItem>,
    },
    Done(BatchDoneInfo),
    /// 关键进度通知（切换城市 / 开始搜索关键词）
    Progress {
        city: String,
        keyword: String,
        status: String,
    },
    /// 不可恢复的致命错误（如切换城市超时），手机端已停止本批次
    FatalError {
        city: String,
        reason: String,
    },
}

// ─── 内部反序列化（serde tagged enum）────────────────────────────────────────

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum RawMsg {
    /// 手机端确认已收到批次请求（字段仅供协议扩展，业务层直接忽略整条消息）
    Ack,
    /// 执行进度通知（切换城市 / 搜索关键词 / 翻页等）
    Progress {
        #[serde(default)]
        city: String,
        #[serde(default)]
        keyword: String,
        #[serde(default)]
        status: String,
    },
    Result {
        city: String,
        keyword: String,
        #[serde(default)]
        items: Vec<ResultItem>,
    },
    Done {
        #[serde(default)]
        total_items: u32,
        #[serde(default)]
        completed_tasks: u32,
        #[serde(default)]
        total_tasks: u32,
        #[serde(default)]
        stopped: bool,
        fatal_reason: Option<String>,
    },
    #[serde(rename = "fatal_error")]
    FatalError {
        #[serde(default)]
        city: String,
        #[serde(default)]
        reason: String,
    },
}

// ─── 错误类型 ─────────────────────────────────────────────────────────────────

/// 扫描过程失败原因（调用方据此决策）
#[derive(Debug)]
pub enum ScanError {
    /// TCP 连接失败：ADB forward 未建立或手机服务未启动
    ConnectionFailed(String),
    /// 连接已建立，但 IO 中途失败（设备断开 / App Crash）
    StreamBroken(String),
    /// 请求序列化失败（内部 BUG，正常不应发生）
    SerializationError(String),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionFailed(e) => write!(f, "连接失败: {}", e),
            Self::StreamBroken(e) => write!(f, "流中断: {}", e),
            Self::SerializationError(e) => write!(f, "序列化失败: {}", e),
        }
    }
}

// ─── NDJSON 流句柄 ────────────────────────────────────────────────────────────

/// 已建立连接的 NDJSON 读取流（逐行按需读取，取消安全）
///
/// Drop 时 TcpStream 自动关闭，手机端会感知到连接断开。
///
/// # 注意
/// `_writer` 持有写半端，防止提前发送 TCP FIN（半关闭）导致手机端误判连接断开。
pub struct PhoneStream {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    _writer: tokio::net::tcp::OwnedWriteHalf,
    line_buf: String,
}

impl PhoneStream {
    /// 读取并解析下一条消息。
    ///
    /// - `Ok(Some(_))` — 收到一条消息
    /// - `Ok(None)` — TCP 连接正常关闭（未收到 `done`，调用方应视为流中断）
    /// - `Err(_)` — IO 错误
    ///
    /// # 健壮性
    /// 单行 JSON 解析失败时记录日志并继续，不中断整个流（允许协议后向扩展新字段 / 新消息类型）。
    pub async fn next_msg(&mut self) -> Result<Option<PhoneMsg>, ScanError> {
        loop {
            self.line_buf.clear();

            let n = self
                .reader
                .read_line(&mut self.line_buf)
                .await
                .map_err(|e| ScanError::StreamBroken(format!("read_line 失败: {}", e)))?;

            if n == 0 {
                // EOF — 连接关闭
                return Ok(None);
            }

            let trimmed = self.line_buf.trim();
            if trimmed.is_empty() {
                continue; // 跳过空行（心跳 / 调试换行）
            }

            match serde_json::from_str::<RawMsg>(trimmed) {
                // ack 静默忽略
                Ok(RawMsg::Ack { .. }) => continue,
                // 关键进度（切换城市 / 开始搜索）向上层传递，其余静默忽略
                Ok(RawMsg::Progress { city, keyword, status }) => {
                    if status == "switching_location" || status == "searching" {
                        return Ok(Some(PhoneMsg::Progress { city, keyword, status }));
                    }
                    continue;
                },
                Ok(RawMsg::Result { city, keyword, items }) => {
                    return Ok(Some(PhoneMsg::Result { city, keyword, items }));
                },
                Ok(RawMsg::Done {
                    total_items,
                    completed_tasks,
                    total_tasks,
                    stopped,
                    fatal_reason,
                }) => {
                    return Ok(Some(PhoneMsg::Done(BatchDoneInfo {
                        total_items,
                        completed_tasks,
                        total_tasks,
                        stopped,
                        fatal_reason,
                    })));
                },
                Ok(RawMsg::FatalError { city, reason, .. }) => {
                    return Ok(Some(PhoneMsg::FatalError { city, reason }));
                },
                Err(e) => {
                    // 协议容错：跳过无法解析的行，记录诊断信息
                    warn!(
                        error = %e,
                        raw = %trimmed.chars().take(160).collect::<String>(),
                        "跳过无法解析的消息"
                    );
                    continue;
                },
            }
        }
    }
}

// ─── 客户端（无状态，可并发复用）─────────────────────────────────────────────

/// 手机无障碍 App TCP 客户端
///
/// 一个 `PhoneClient` 实例对应一个 PC 本地端口（即一台手机设备）。
/// 无内部状态，可在多次任务间复用（每次调用 `open` 建立新连接）。
pub struct PhoneClient {
    local_port: u16,
}

impl PhoneClient {
    /// 创建客户端实例。`local_port` 为 PC 本地监听端口（经 ADB forward 映射到手机 7899）。
    pub fn new(local_port: u16) -> Self {
        Self { local_port }
    }

    /// 轻量健康检测：TCP 连接 + 发送 ping 请求 + 等待 ack 响应。
    ///
    /// 用于在调度前主动确认手机端无障碍 App 是否在线且正常响应。
    /// 比 `open` 更轻量：不发送批次任务，只验证连通性。
    ///
    /// # 超时
    /// 使用 `phone_client::PING_TIMEOUT_SECS`（3s），远短于正常连接超时。
    pub async fn ping(&self) -> Result<(), ScanError> {
        let addr = format!("127.0.0.1:{}", self.local_port);

        // 1. TCP 连接（短超时）
        let stream = tokio::time::timeout(
            Duration::from_secs(cfg::PING_TIMEOUT_SECS),
            TcpStream::connect(&addr),
        )
        .await
        .map_err(|_| {
            ScanError::ConnectionFailed(format!(
                "ping 超时 ({}s): {}",
                cfg::PING_TIMEOUT_SECS,
                addr
            ))
        })?
        .map_err(|e| ScanError::ConnectionFailed(format!("ping connect 失败 {}: {}", addr, e)))?;

        let (reader_half, mut writer_half) = stream.into_split();

        // 2. 发送 ping 请求
        let payload = "{\"type\":\"ping\"}\n";
        writer_half
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| ScanError::StreamBroken(format!("ping write 失败: {}", e)))?;
        writer_half
            .flush()
            .await
            .map_err(|e| ScanError::StreamBroken(format!("ping flush 失败: {}", e)))?;

        // 3. 等待 ack 响应（带超时）
        let mut reader = BufReader::new(reader_half);
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(cfg::PING_TIMEOUT_SECS),
            reader.read_line(&mut line),
        )
        .await
        .map_err(|_| {
            ScanError::ConnectionFailed(format!("ping ack 超时 ({}s)", cfg::PING_TIMEOUT_SECS))
        })?
        .map_err(|e| ScanError::StreamBroken(format!("ping read 失败: {}", e)))?;

        Ok(())
    }

    /// 建立 TCP 连接并发送批次请求，返回 NDJSON 流句柄。
    ///
    /// # 超时
    /// 连接阶段受 `phone_client::CONNECT_TIMEOUT_SECS` 控制。
    /// 读取阶段无独立超时（由调用方通过 `CancellationToken` 控制）。
    ///
    /// # 错误
    /// - `ConnectionFailed` — TCP 连接超时或被拒绝
    /// - `StreamBroken` — 连接成功但写入批次请求失败
    /// - `SerializationError` — JSON 序列化内部错误（不应发生）
    pub async fn open(
        &self,
        batch_id: &str,
        tasks: &[BatchTask],
        max_pages: u32,
    ) -> Result<PhoneStream, ScanError> {
        let addr = format!("127.0.0.1:{}", self.local_port);

        // ── 1. TCP 连接（带超时）──
        let stream = tokio::time::timeout(
            Duration::from_secs(cfg::CONNECT_TIMEOUT_SECS),
            TcpStream::connect(&addr),
        )
        .await
        .map_err(|_| {
            ScanError::ConnectionFailed(format!(
                "连接超时 ({}s): {}",
                cfg::CONNECT_TIMEOUT_SECS,
                addr
            ))
        })?
        .map_err(|e| ScanError::ConnectionFailed(format!("connect 失败 {}: {}", addr, e)))?;

        // TCP_NODELAY：减少小包延迟（请求行仅几百字节）
        if let Err(e) = stream.set_nodelay(true) {
            debug!(error = %e, "设置 TCP_NODELAY 失败 (非致命)");
        }

        let (reader_half, mut writer_half) = stream.into_split();

        // ── 2. 序列化批次请求 ──
        let req = BatchRequest { r#type: "batch", batch_id, tasks, max_pages };
        let mut payload = serde_json::to_string(&req)
            .map_err(|e| ScanError::SerializationError(format!("序列化失败: {}", e)))?;
        payload.push('\n');

        // ── 3. 发送请求行 ──
        writer_half
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| ScanError::StreamBroken(format!("write_all 失败: {}", e)))?;

        writer_half
            .flush()
            .await
            .map_err(|e| ScanError::StreamBroken(format!("flush 失败: {}", e)))?;

        debug!(
            batch_id = batch_id,
            task_count = tasks.len(),
            port = self.local_port,
            "批次请求已发送"
        );

        Ok(PhoneStream {
            reader: BufReader::new(reader_half),
            _writer: writer_half,
            line_buf: String::new(),
        })
    }
}
