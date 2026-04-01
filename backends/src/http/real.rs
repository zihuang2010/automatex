//! 真实 HTTP 客户端 — reqwest 实现
//!
//! 当 `api_base_url` 非空时启用。
//! 后续逐个接入真实 API 时只需在此文件中完善 trait 方法。

use async_trait::async_trait;
use reqwest::Method;
use std::time::Instant;

use super::types::*;
use super::ApiClient;
use crate::constants;
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
        T: serde::de::DeserializeOwned + Default,
    {
        const MAX_ATTEMPTS: usize = 3;

        for attempt in 1..=MAX_ATTEMPTS {
            let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
            let mut request = self.client.request(method.clone(), &url);
            if let Some(ref payload) = body {
                eprintln!(
                    "[http] {} 请求: method={}, url={}, body={}",
                    action, method, url, payload
                );
                request = request.json(payload);
            } else {
                eprintln!("[http] {} 请求: method={}, url={}, body=<empty>", action, method, url);
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
                        eprintln!(
                            "[http] {} 服务端错误，准备重试: status={}, attempt={}/{}, elapsed_ms={}, backoff_ms={}",
                            action,
                            status,
                            attempt,
                            MAX_ATTEMPTS,
                            started_at.elapsed().as_millis(),
                            backoff_ms
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }

                    let text =
                        resp.text().await.map_err(|e| format!("{} 读取响应失败: {}", action, e))?;
                    eprintln!(
                        "[http] {} 响应: status={}, elapsed_ms={}, body={}",
                        action,
                        status,
                        started_at.elapsed().as_millis(),
                        text
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
                        eprintln!("[http] {} 响应 data 为 null，使用默认值兼容", action);
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
                        eprintln!(
                            "[http] {} 传输失败，准备重试: err={}, attempt={}/{}, elapsed_ms={}, backoff_ms={}",
                            action,
                            err,
                            attempt,
                            MAX_ATTEMPTS,
                            started_at.elapsed().as_millis(),
                            backoff_ms
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
