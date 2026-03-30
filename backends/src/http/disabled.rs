use async_trait::async_trait;

use super::types::*;
use super::ApiClient;
use crate::task_provider::TaskDef;

pub struct DisabledApiClient {
    reason: String,
}

impl DisabledApiClient {
    pub fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }

    fn err(&self, action: &str) -> String {
        format!("[http] {} 不可用: {}", action, self.reason)
    }
}

#[async_trait]
impl ApiClient for DisabledApiClient {
    async fn device_sync(&self, _req: &DeviceSyncRequest) -> Result<DeviceSyncResponse, String> {
        Err(self.err("device_sync"))
    }

    async fn bind_phones(&self, _req: &PhoneBindRequest) -> Result<PhoneBindResponse, String> {
        Err(self.err("bind_phones"))
    }

    async fn fetch_tasks_by_phones(
        &self,
        _client_id: &str,
        _phones: &[String],
    ) -> Result<PhoneTasksResponse, String> {
        Err(self.err("fetch_tasks_by_phones"))
    }

    async fn fetch_task(&self, _task_id: &str) -> Result<TaskDef, String> {
        Err(self.err("fetch_task"))
    }

    async fn report_progress(&self, _req: &ProgressReportRequest) -> Result<ApiResponse, String> {
        Err(self.err("report_progress"))
    }

    async fn unbind_phones(
        &self,
        _client_id: &str,
        _phones: &[String],
    ) -> Result<ApiResponse, String> {
        Err(self.err("unbind_phones"))
    }
}
