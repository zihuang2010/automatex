//! HTTP 客户端模块
//!
//! 定义 `ApiClient` trait 和 工厂函数。
//! - `api_base_url` 为空 → `MockApiClient`（本地模拟）
//! - `api_base_url` 非空 → `RealApiClient`（reqwest 真实请求）
//!
//! 所有业务代码依赖 `dyn ApiClient`，切换实现无需修改调用方。

mod disabled;
mod mock;
mod real;
pub mod types;

pub use disabled::DisabledApiClient;
pub use mock::MockApiClient;
pub use real::RealApiClient;
pub use types::*;

use crate::task_provider::TaskDef;
use async_trait::async_trait;

/// HTTP API 客户端 trait —— 所有业务代码依赖此 trait
#[async_trait]
pub trait ApiClient: Send + Sync {
    /// 设备归属同步
    async fn device_sync(&self, req: &DeviceSyncRequest) -> Result<DeviceSyncResponse, String>;

    /// 手机号绑定（互斥策略）
    async fn bind_phones(&self, req: &PhoneBindRequest) -> Result<PhoneBindResponse, String>;

    /// 按手机号拉取任务列表
    async fn fetch_tasks_by_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<PhoneTasksResponse, String>;

    /// 拉取单个任务定义
    async fn fetch_task(&self, task_id: &str) -> Result<TaskDef, String>;

    /// 上报关键词完成进度
    async fn report_progress(&self, req: &ProgressReportRequest) -> Result<ApiResponse, String>;

    /// 解绑手机号
    async fn unbind_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<ApiResponse, String>;
}
