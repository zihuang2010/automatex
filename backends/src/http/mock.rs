//! Mock HTTP 客户端 — 本地模拟所有 API 行为
//!
//! 当 `api_base_url` 为空时自动启用。
//! 支持从 `~/.automatex/mock/scenarios/{name}.json` 加载自定义场景。

use async_trait::async_trait;

use super::types::*;
use super::ApiClient;
use crate::task_provider::TaskDef;

/// Mock 实现
pub struct MockApiClient {
    /// 当前 mock 场景名（对应 resources/mock/scenarios/{name}.json）
    mock_scenario: std::sync::Mutex<String>,
}

impl MockApiClient {
    pub fn new() -> Self {
        eprintln!("[http] Mock 模式启用（服务端 URL 未配置）");
        Self { mock_scenario: std::sync::Mutex::new("default".to_string()) }
    }

    /// 设置 mock 场景（运行时切换）
    pub fn set_mock_scenario(&self, name: &str) {
        if let Ok(mut s) = self.mock_scenario.lock() {
            eprintln!("[http-mock] 切换场景: {} → {}", *s, name);
            *s = name.to_string();
        }
    }

    /// 加载当前 mock 场景 JSON
    fn load_mock_scenario(&self) -> Option<serde_json::Value> {
        let name = self.mock_scenario.lock().ok()?.clone();
        // 优先从 ~/.automatex/mock/scenarios/ 加载（用户自定义）
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        let user_path = format!("{}/.automatex/mock/scenarios/{}.json", home, name);
        if let Ok(content) = std::fs::read_to_string(&user_path) {
            eprintln!("[http-mock] 加载用户场景: {}", user_path);
            return serde_json::from_str(&content).ok();
        }

        // fallback: 从嵌入资源中读取
        let resource_path =
            format!("{}/resources/mock/scenarios/{}.json", env!("CARGO_MANIFEST_DIR"), name);
        if let Ok(content) = std::fs::read_to_string(&resource_path) {
            eprintln!("[http-mock] 加载内置场景: {}", resource_path);
            return serde_json::from_str(&content).ok();
        }

        eprintln!("[http-mock] 场景文件未找到: {}, 使用内置默认", name);
        None
    }
}

#[async_trait]
impl ApiClient for MockApiClient {
    async fn device_sync(&self, req: &DeviceSyncRequest) -> Result<DeviceSyncResponse, String> {
        eprintln!(
            "[http-mock] device_sync: client={}, online={}, offline={}",
            req.client_id,
            req.online.len(),
            req.offline_local.len()
        );
        Ok(DeviceSyncResponse { to_remove: Vec::new() })
    }

    async fn bind_phones(&self, req: &PhoneBindRequest) -> Result<PhoneBindResponse, String> {
        eprintln!(
            "[http-mock] bind_phones: client={}, phones={:?}, force={}",
            req.client_id, req.phones, req.force
        );

        if let Some(scenario) = self.load_mock_scenario() {
            if let Some(bind_data) = scenario.get("bind_phones") {
                let resp: PhoneBindResponse = serde_json::from_value(bind_data.clone())
                    .unwrap_or_else(|e| {
                        eprintln!("[http-mock] 解析 bind_phones 场景失败: {}", e);
                        PhoneBindResponse { bound: req.phones.clone(), conflicts: Vec::new() }
                    });
                eprintln!(
                    "[http-mock] bind_phones 场景响应: bound={}, conflicts={}",
                    resp.bound.len(),
                    resp.conflicts.len()
                );
                return Ok(resp);
            }
        }

        Ok(PhoneBindResponse { bound: req.phones.clone(), conflicts: Vec::new() })
    }

    async fn fetch_tasks_by_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<PhoneTasksResponse, String> {
        eprintln!("[http-mock] fetch_tasks_by_phones: client={}, phones={:?}", client_id, phones);

        // 优先从场景 JSON 的 phone_tasks 字段读取
        if let Some(scenario) = self.load_mock_scenario() {
            if let Some(pt) = scenario.get("phone_tasks") {
                if let Ok(phone_tasks) = serde_json::from_value::<
                    std::collections::HashMap<String, Vec<TaskDef>>,
                >(pt.clone())
                {
                    let filtered: std::collections::HashMap<String, Vec<TaskDef>> = phone_tasks
                        .into_iter()
                        .filter(|(phone, _)| phones.contains(phone))
                        .collect();
                    let total: usize = filtered.values().map(|v| v.len()).sum();
                    eprintln!(
                        "[http-mock] 场景返回: {} 个手机号, {} 个任务",
                        filtered.len(),
                        total
                    );
                    return Ok(PhoneTasksResponse { phone_tasks: filtered });
                }
            }
        }

        // fallback: 从 mock_tasks.json 加载，round-robin 分配
        use crate::task_provider::load_mock_definitions;
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

    async fn fetch_task(&self, task_id: &str) -> Result<TaskDef, String> {
        use crate::task_provider::load_mock_task_def_by_id;
        eprintln!("[http-mock] fetch_task: task_id={}", task_id);
        load_mock_task_def_by_id(task_id).ok_or_else(|| format!("任务不存在: {}", task_id))
    }

    async fn report_progress(&self, req: &ProgressReportRequest) -> Result<ApiResponse, String> {
        eprintln!(
            "[http-mock] report_progress: task={}, city={}, kw={}, status={}",
            req.task_id, req.city_name, req.keyword_name, req.status
        );
        Ok(ApiResponse { success: true, message: "mock: ok".to_string() })
    }

    async fn unbind_phones(
        &self,
        client_id: &str,
        phones: &[String],
    ) -> Result<ApiResponse, String> {
        eprintln!("[http-mock] unbind_phones: client={}, phones={}", client_id, phones.len());
        Ok(ApiResponse { success: true, message: "mock: ok".to_string() })
    }
}
