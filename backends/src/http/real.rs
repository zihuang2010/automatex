//! 真实 HTTP 客户端 — reqwest 实现
//!
//! 当 `api_base_url` 非空时启用。
//! 后续逐个接入真实 API 时只需在此文件中完善 trait 方法。

use async_trait::async_trait;
use reqwest::Method;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tracing::{debug, error, info, warn};

use super::types::*;
use super::ApiClient;
use crate::constants;
/// 真实 HTTP 实现
pub struct RealApiClient {
    client: reqwest::Client,
    base_url: String,
}

static NEXT_HTTP_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

impl RealApiClient {
    pub fn new(base_url: &str) -> Self {
        info!(base_url = %base_url, "真实模式启用");
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(
                constants::timing::HTTP_CONNECT_TIMEOUT_SECS,
            ))
            .timeout(std::time::Duration::from_secs(constants::timing::HTTP_REQUEST_TIMEOUT_SECS))
            .tcp_keepalive(Some(std::time::Duration::from_secs(30)))
            .pool_idle_timeout(Some(std::time::Duration::from_secs(90)))
            .pool_max_idle_per_host(8)
            .user_agent(format!("automatex/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|e| {
                error!(error = %e, "构建 reqwest client 失败，退回默认配置");
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
        T: serde::de::DeserializeOwned + Default,
    {
        const MAX_ATTEMPTS: usize = 3;
        let request_id = NEXT_HTTP_REQUEST_ID.fetch_add(1, Ordering::Relaxed);

        for attempt in 1..=MAX_ATTEMPTS {
            let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
            let mut request = self.client.request(method.clone(), &url);
            if let Some(ref payload) = body {
                debug!(
                    request_id,
                    action,
                    method = %method,
                    url = %url,
                    body = %payload,
                    "请求"
                );
                request = request.json(payload);
            } else {
                debug!(
                    request_id,
                    action,
                    method = %method,
                    url = %url,
                    body = "<empty>",
                    "请求"
                );
            }

            let started_at = Instant::now();
            match request.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if (status.is_server_error() || status.as_u16() == 429)
                        && attempt < MAX_ATTEMPTS
                    {
                        let backoff_ms = constants::timing::HTTP_RETRY_BASE_DELAY_MS
                            * attempt as u64
                            * attempt as u64;
                        warn!(
                            request_id,
                            action,
                            status = %status,
                            attempt,
                            max_attempts = MAX_ATTEMPTS,
                            elapsed_ms = started_at.elapsed().as_millis() as u64,
                            backoff_ms,
                            "服务端错误，准备重试"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }

                    let text =
                        resp.text().await.map_err(|e| format!("{} 读取响应失败: {}", action, e))?;
                    debug!(
                        request_id,
                        action,
                        status = %status,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        body = %text,
                        "响应"
                    );
                    if !status.is_success() {
                        return Err(format!(
                            "{} 请求失败: status={}, body={}",
                            action, status, text
                        ));
                    }
                    let envelope = serde_json::from_str::<ApiEnvelope<T>>(&text)
                        .map_err(|e| format!("{} 解析失败: {}, raw={}", action, e, text))?;
                    let _ = envelope.service_code;
                    if envelope.code != 1 {
                        return Err(format!("{} 失败: {}", action, envelope.msg));
                    }
                    if envelope.data.is_none() {
                        warn!(action, "响应 data 为 null，使用默认值兼容");
                    }
                    return Ok(envelope.data.unwrap_or_default());
                },
                Err(err) => {
                    let retryable = err.is_timeout()
                        || err.is_connect()
                        || err
                            .status()
                            .map(|status| status.is_server_error() || status.as_u16() == 429)
                            .unwrap_or(false);
                    if retryable && attempt < MAX_ATTEMPTS {
                        let backoff_ms = constants::timing::HTTP_RETRY_BASE_DELAY_MS
                            * attempt as u64
                            * attempt as u64;
                        warn!(
                            request_id,
                            action,
                            error = %err,
                            attempt,
                            max_attempts = MAX_ATTEMPTS,
                            elapsed_ms = started_at.elapsed().as_millis() as u64,
                            backoff_ms,
                            "传输失败，准备重试"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                    return Err(format!(
                        "{} 请求失败: {}, elapsed_ms={}",
                        action,
                        err,
                        started_at.elapsed().as_millis()
                    ));
                },
            }
        }

        Err(format!("{} 请求失败: 已达到最大重试次数", action))
    }
}

#[async_trait]
impl ApiClient for RealApiClient {
    async fn bind_phones(&self, req: &PhoneBindRequest) -> Result<PhoneBindResponse, String> {
        self.send_json(
            Method::POST,
            "/mttl_tools/v1/meituanTraffic/client/bind",
            Some(serde_json::to_value(req).map_err(|e| format!("bind_phones 序列化失败: {}", e))?),
            "bind_phones",
        )
        .await
    }

    async fn batch_fetch_tasks(
        &self,
        req: &BatchTasksRequest,
    ) -> Result<Vec<BatchTaskItem>, String> {
        self.send_json(
            Method::POST,
            "/mttl_tools/v1/meituanTraffic/client/batchTasks",
            Some(
                serde_json::to_value(req)
                    .map_err(|e| format!("batch_fetch_tasks 序列化失败: {}", e))?,
            ),
            "batch_fetch_tasks",
        )
        .await
    }

    async fn report_progress(&self, req: &ProgressReportRequest) -> Result<ApiResponse, String> {
        self.send_json(
            Method::POST,
            "/mttl_tools/v1/meituanTraffic/client/scan/upload",
            Some(
                serde_json::to_value(req)
                    .map_err(|e| format!("report_progress 序列化失败: {}", e))?,
            ),
            "report_progress",
        )
        .await
    }

    async fn unbind_phones(&self, req: &UnbindPhonesRequest) -> Result<ApiResponse, String> {
        self.send_json(
            Method::POST,
            "/mttl_tools/v1/meituanTraffic/client/unbind",
            Some(
                serde_json::to_value(req)
                    .map_err(|e| format!("unbind_phones 序列化失败: {}", e))?,
            ),
            "unbind_phones",
        )
        .await
    }
}
