use async_trait::async_trait;

use super::types::*;
use super::ApiClient;
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
    async fn bind_phones(&self, _req: &PhoneBindRequest) -> Result<PhoneBindResponse, String> {
        Err(self.err("bind_phones"))
    }

    async fn batch_fetch_tasks(
        &self,
        _req: &BatchTasksRequest,
    ) -> Result<Vec<BatchTaskItem>, String> {
        Err(self.err("batch_fetch_tasks"))
    }

    async fn report_progress(&self, _req: &ProgressReportRequest) -> Result<ApiResponse, String> {
        Err(self.err("report_progress"))
    }

    async fn unbind_phones(&self, _req: &UnbindPhonesRequest) -> Result<ApiResponse, String> {
        Err(self.err("unbind_phones"))
    }
}
