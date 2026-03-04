use serde::{Deserialize, Serialize};

// ─── 设备同步 ────────────────────────────────────────────────────

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

// ─── 进度上报 ────────────────────────────────────────────────────

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
    pub success: bool,
    pub message: String,
}

// ─── 手机号绑定 ──────────────────────────────────────────────────

/// 手机号绑定请求
#[derive(Debug, Serialize)]
pub struct PhoneBindRequest {
    pub client_id: String,
    pub phones: Vec<String>,
    pub force: bool,
}

/// 手机号绑定响应
#[derive(Debug, Deserialize, Serialize)]
pub struct PhoneBindResponse {
    pub bound: Vec<String>,
    pub conflicts: Vec<PhoneConflict>,
}

/// 手机号冲突详情
#[derive(Debug, Deserialize, Serialize)]
pub struct PhoneConflict {
    pub phone: String,
    pub current_client: String,
}

/// 按手机号获取任务列表响应
#[derive(Debug, Deserialize)]
pub struct PhoneTasksResponse {
    pub phone_tasks: std::collections::HashMap<String, Vec<crate::task_provider::TaskDef>>,
}
