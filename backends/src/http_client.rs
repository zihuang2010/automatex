//! HTTP 客户端模块
//!
//! 当前服务端 API 未就绪，所有请求使用 Mock JSON 响应。
//! 后续切换为真实 HTTP 时只需修改此模块，不影响调用方。

use serde::{Deserialize, Serialize};

use crate::task_provider::TaskDef;

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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
    #[allow(dead_code)]
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

    /// 拉取单个任务的最新定义
    /// Mock 模式：从 mock_tasks.json 中查找；真实模式：GET /api/tasks/{task_id}
    pub async fn fetch_task(&self, task_id: &str) -> Result<TaskDef, String> {
        if self.mock_mode {
            return self.mock_fetch_task(task_id);
        }

        // TODO: 真实 HTTP 请求
        // let url = format!("{}/api/tasks/{}", self.base_url, task_id);
        // let resp = reqwest::Client::new().get(&url).send().await...
        Err("HTTP 客户端未实现真实请求".into())
    }

    fn mock_fetch_task(&self, task_id: &str) -> Result<TaskDef, String> {
        use crate::task_provider::load_mock_task_def_by_id;
        eprintln!("[http-mock] fetch_task: task_id={}", task_id);
        load_mock_task_def_by_id(task_id).ok_or_else(|| format!("任务不存在: {}", task_id))
    }

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

    // ─── 手机号绑定接口 ───────────────────────────────────────────

    /// 手机号绑定（互斥策略）
    pub async fn bind_phones(&self, req: &PhoneBindRequest) -> Result<PhoneBindResponse, String> {
        if self.mock_mode {
            return self.mock_bind_phones(req);
        }
        // TODO: POST {base_url}/api/phones/bind
        Err("HTTP 客户端未实现真实请求".into())
    }

    /// 按手机号获取任务列表
    pub async fn fetch_tasks_by_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<PhoneTasksResponse, String> {
        if self.mock_mode {
            return self.mock_fetch_tasks_by_phones(client_id, phones);
        }
        // TODO: POST {base_url}/api/tasks/by-phones
        Err("HTTP 客户端未实现真实请求".into())
    }

    /// 解绑手机号
    #[allow(dead_code)]
    pub async fn unbind_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<ApiResponse, String> {
        if self.mock_mode {
            return self.mock_unbind_phones(client_id, phones);
        }
        // TODO: POST {base_url}/api/phones/unbind
        Err("HTTP 客户端未实现真实请求".into())
    }

    fn mock_bind_phones(&self, req: &PhoneBindRequest) -> Result<PhoneBindResponse, String> {
        eprintln!(
            "[http-mock] bind_phones: client={}, phones={}, force={}",
            req.client_id,
            req.phones.len(),
            req.force
        );
        // Mock: 全部绑定成功，无冲突
        Ok(PhoneBindResponse { bound: req.phones.clone(), conflicts: Vec::new() })
    }

    fn mock_fetch_tasks_by_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<PhoneTasksResponse, String> {
        use crate::task_provider::load_mock_definitions;
        eprintln!(
            "[http-mock] fetch_tasks_by_phones: client={}, phones={}",
            client_id,
            phones.len()
        );
        // Mock: 将所有 mock 任务平均分配给手机号
        let all_defs = load_mock_definitions();
        let mut phone_tasks: std::collections::HashMap<String, Vec<TaskDef>> =
            std::collections::HashMap::new();
        for (i, def) in all_defs.into_iter().enumerate() {
            if !phones.is_empty() {
                let phone = &phones[i % phones.len()];
                phone_tasks.entry(phone.clone()).or_default().push(def);
            }
        }
        Ok(PhoneTasksResponse { phone_tasks })
    }

    #[allow(dead_code)]
    fn mock_unbind_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<ApiResponse, String> {
        eprintln!("[http-mock] unbind_phones: client={}, phones={}", client_id, phones.len());
        Ok(ApiResponse { success: true, message: "mock: ok".to_string() })
    }
}

// ─── 手机号绑定相关数据结构 ───────────────────────────────────────

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
    pub phone_tasks: std::collections::HashMap<String, Vec<TaskDef>>,
}
