//! 真实 HTTP 客户端 — reqwest 实现
//!
//! 当 `api_base_url` 非空时启用。
//! 后续逐个接入真实 API 时只需在此文件中完善 trait 方法。

use async_trait::async_trait;

use super::types::*;
use super::ApiClient;
use crate::task_provider::TaskDef;

/// 真实 HTTP 实现
pub struct RealApiClient {
    client: reqwest::Client,
    base_url: String,
}

impl RealApiClient {
    pub fn new(base_url: &str) -> Self {
        eprintln!("[http] 真实模式启用: {}", base_url);
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

#[async_trait]
impl ApiClient for RealApiClient {
    async fn device_sync(&self, req: &DeviceSyncRequest) -> Result<DeviceSyncResponse, String> {
        let resp = self
            .client
            .post(format!("{}/api/devices/sync", self.base_url))
            .json(req)
            .send()
            .await
            .map_err(|e| format!("device_sync 请求失败: {}", e))?;

        resp.json().await.map_err(|e| format!("device_sync 解析失败: {}", e))
    }

    async fn bind_phones(&self, req: &PhoneBindRequest) -> Result<PhoneBindResponse, String> {
        let resp = self
            .client
            .post(format!("{}/api/phones/bind", self.base_url))
            .json(req)
            .send()
            .await
            .map_err(|e| format!("bind_phones 请求失败: {}", e))?;

        resp.json().await.map_err(|e| format!("bind_phones 解析失败: {}", e))
    }

    async fn fetch_tasks_by_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<PhoneTasksResponse, String> {
        let resp = self
            .client
            .post(format!("{}/api/tasks/by-phones", self.base_url))
            .json(&serde_json::json!({
                "client_id": client_id,
                "phones": phones,
            }))
            .send()
            .await
            .map_err(|e| format!("fetch_tasks_by_phones 请求失败: {}", e))?;

        resp.json().await.map_err(|e| format!("fetch_tasks_by_phones 解析失败: {}", e))
    }

    async fn fetch_task(&self, task_id: &str) -> Result<TaskDef, String> {
        let resp = self
            .client
            .get(format!("{}/api/tasks/{}", self.base_url, task_id))
            .send()
            .await
            .map_err(|e| format!("fetch_task 请求失败: {}", e))?;

        resp.json().await.map_err(|e| format!("fetch_task 解析失败: {}", e))
    }

    async fn report_progress(&self, req: &ProgressReportRequest) -> Result<ApiResponse, String> {
        let resp = self
            .client
            .post(format!("{}/api/progress/report", self.base_url))
            .json(req)
            .send()
            .await
            .map_err(|e| format!("report_progress 请求失败: {}", e))?;

        resp.json().await.map_err(|e| format!("report_progress 解析失败: {}", e))
    }

    async fn unbind_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<ApiResponse, String> {
        let resp = self
            .client
            .post(format!("{}/api/phones/unbind", self.base_url))
            .json(&serde_json::json!({
                "client_id": client_id,
                "phones": phones,
            }))
            .send()
            .await
            .map_err(|e| format!("unbind_phones 请求失败: {}", e))?;

        resp.json().await.map_err(|e| format!("unbind_phones 解析失败: {}", e))
    }
}
