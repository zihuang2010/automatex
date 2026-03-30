//! 真实 HTTP 客户端 — reqwest 实现
//!
//! 当 `api_base_url` 非空时启用。
//! 后续逐个接入真实 API 时只需在此文件中完善 trait 方法。

use async_trait::async_trait;
use reqwest::Method;

use super::types::*;
use super::ApiClient;
use crate::constants;
use crate::task_provider::TaskDef;

/// 真实 HTTP 实现
pub struct RealApiClient {
    client: reqwest::Client,
    base_url: String,
}

impl RealApiClient {
    pub fn new(base_url: &str) -> Self {
        eprintln!("[http] 真实模式启用: {}", base_url);
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(
                constants::timing::HTTP_CONNECT_TIMEOUT_SECS,
            ))
            .timeout(std::time::Duration::from_secs(constants::timing::HTTP_REQUEST_TIMEOUT_SECS))
            .tcp_keepalive(Some(std::time::Duration::from_secs(30)))
            .pool_idle_timeout(Some(std::time::Duration::from_secs(90)))
            .build()
            .unwrap_or_else(|e| {
                eprintln!("[http] 构建 reqwest client 失败，退回默认配置: {}", e);
                reqwest::Client::new()
            });
        Self { client, base_url: base_url.trim_end_matches('/').to_string() }
    }

    async fn send_json<T>(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
        action: &str,
    ) -> Result<T, String>
    where
        T: serde::de::DeserializeOwned,
    {
        const MAX_ATTEMPTS: usize = 2;

        for attempt in 1..=MAX_ATTEMPTS {
            let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
            let mut request = self.client.request(method.clone(), &url);
            if let Some(ref payload) = body {
                request = request.json(payload);
            }

            match request.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_server_error() && attempt < MAX_ATTEMPTS {
                        eprintln!(
                            "[http] {} 服务端错误，准备重试: status={}, attempt={}/{}",
                            action, status, attempt, MAX_ATTEMPTS
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(300 * attempt as u64))
                            .await;
                        continue;
                    }

                    let resp = resp.error_for_status().map_err(|e| {
                        format!("{} 请求失败: status={}, err={}", action, status, e)
                    })?;
                    return resp
                        .json::<T>()
                        .await
                        .map_err(|e| format!("{} 解析失败: {}", action, e));
                },
                Err(err) => {
                    let retryable = err.is_timeout()
                        || err.is_connect()
                        || err.status().map(|status| status.is_server_error()).unwrap_or(false);
                    if retryable && attempt < MAX_ATTEMPTS {
                        eprintln!(
                            "[http] {} 传输失败，准备重试: err={}, attempt={}/{}",
                            action, err, attempt, MAX_ATTEMPTS
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(300 * attempt as u64))
                            .await;
                        continue;
                    }
                    return Err(format!("{} 请求失败: {}", action, err));
                },
            }
        }

        Err(format!("{} 请求失败: 已达到最大重试次数", action))
    }
}

#[async_trait]
impl ApiClient for RealApiClient {
    async fn device_sync(&self, req: &DeviceSyncRequest) -> Result<DeviceSyncResponse, String> {
        self.send_json(
            Method::POST,
            "/api/devices/sync",
            Some(serde_json::to_value(req).map_err(|e| format!("device_sync 序列化失败: {}", e))?),
            "device_sync",
        )
        .await
    }

    async fn bind_phones(&self, req: &PhoneBindRequest) -> Result<PhoneBindResponse, String> {
        self.send_json(
            Method::POST,
            "/api/phones/bind",
            Some(serde_json::to_value(req).map_err(|e| format!("bind_phones 序列化失败: {}", e))?),
            "bind_phones",
        )
        .await
    }

    async fn fetch_tasks_by_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<PhoneTasksResponse, String> {
        self.send_json(
            Method::POST,
            "/api/tasks/by-phones",
            Some(serde_json::json!({
                "client_id": client_id,
                "phones": phones,
            })),
            "fetch_tasks_by_phones",
        )
        .await
    }

    async fn fetch_task(&self, task_id: &str) -> Result<TaskDef, String> {
        self.send_json(Method::GET, &format!("/api/tasks/{}", task_id), None, "fetch_task")
            .await
    }

    async fn report_progress(&self, req: &ProgressReportRequest) -> Result<ApiResponse, String> {
        self.send_json(
            Method::POST,
            "/api/progress/report",
            Some(
                serde_json::to_value(req)
                    .map_err(|e| format!("report_progress 序列化失败: {}", e))?,
            ),
            "report_progress",
        )
        .await
    }

    async fn unbind_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<ApiResponse, String> {
        self.send_json(
            Method::POST,
            "/api/phones/unbind",
            Some(serde_json::json!({
                "client_id": client_id,
                "phones": phones,
            })),
            "unbind_phones",
        )
        .await
    }
}
