//! HTTP 客户端模块
//!
//! 当前服务端 API 未就绪，所有请求使用 Mock JSON 响应。
//! 后续切换为真实 HTTP 时只需修改此模块，不影响调用方。

use serde::{Deserialize, Serialize};

// ─── 请求/响应数据结构 ───────────────────────────────────────────

/// 启动同步请求 — 检查设备归属
#[derive(Debug, Serialize)]
pub struct DeviceSyncRequest {
    pub client_id: String,
    pub online: Vec<DeviceSyncItem>,
    pub offline_local: Vec<String>, // hw_serial 列表
}

#[derive(Debug, Serialize)]
pub struct DeviceSyncItem {
    pub hw_serial: String,
    pub serial: String,
    pub state: String,
}

/// 启动同步响应
#[derive(Debug, Deserialize)]
pub struct DeviceSyncResponse {
    /// 需要从本地删除的设备 hw_serial
    pub to_remove: Vec<String>,
}

/// 进度上报请求
#[derive(Debug, Serialize)]
pub struct ProgressReportRequest {
    pub client_id: String,
    pub task_id: String,
    pub city_name: String,
    pub keyword_name: String,
    pub device_serial: String,
    pub status: String,
    pub completed_at: i64,
}

/// 通用 API 响应
#[derive(Debug, Deserialize)]
pub struct ApiResponse {
    #[allow(dead_code)]
    pub success: bool,
    #[allow(dead_code)]
    pub message: String,
}

// ─── HTTP 客户端 ─────────────────────────────────────────────────

pub struct HttpClient {
    #[allow(dead_code)]
    base_url: String,
    mock_mode: bool,
}

impl HttpClient {
    /// 创建 HTTP 客户端
    /// 当 base_url 为空时自动启用 mock 模式
    pub fn new(base_url: &str) -> Self {
        let mock_mode = base_url.is_empty();
        if mock_mode {
            eprintln!("[http] Mock 模式启用（服务端 URL 未配置）");
        }
        Self { base_url: base_url.to_string(), mock_mode }
    }

    /// 启动同步 — 检查设备归属
    /// 发送本地在线+离线设备列表，服务端返回需要删除的设备
    pub async fn device_sync(&self, req: &DeviceSyncRequest) -> Result<DeviceSyncResponse, String> {
        if self.mock_mode {
            return self.mock_device_sync(req);
        }

        // TODO: 真实 HTTP 请求
        // let url = format!("{}/api/devices/sync", self.base_url);
        // let resp = reqwest::Client::new().post(&url).json(req).send().await...
        Err("HTTP 客户端未实现真实请求".into())
    }

    /// 上报关键词完成进度
    pub async fn report_progress(
        &self,
        req: &ProgressReportRequest,
    ) -> Result<ApiResponse, String> {
        if self.mock_mode {
            return self.mock_report_progress(req);
        }

        // TODO: 真实 HTTP 请求
        Err("HTTP 客户端未实现真实请求".into())
    }

    // ─── Mock 实现 ───────────────────────────────────────────────

    fn mock_device_sync(&self, req: &DeviceSyncRequest) -> Result<DeviceSyncResponse, String> {
        eprintln!(
            "[http-mock] device_sync: client={}, online={}, offline={}",
            req.client_id,
            req.online.len(),
            req.offline_local.len()
        );

        // Mock 逻辑：不删除任何设备（没有其他客户端竞争）
        Ok(DeviceSyncResponse { to_remove: Vec::new() })
    }

    fn mock_report_progress(&self, req: &ProgressReportRequest) -> Result<ApiResponse, String> {
        eprintln!(
            "[http-mock] report_progress: task={}, city={}, kw={}, status={}",
            req.task_id, req.city_name, req.keyword_name, req.status
        );

        Ok(ApiResponse { success: true, message: "mock: ok".to_string() })
    }
}
