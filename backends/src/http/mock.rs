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
    async fn bind_phones(&self, req: &PhoneBindRequest) -> Result<PhoneBindResponse, String> {
        eprintln!(
            "[http-mock] bind_phones: client={}, mobiles={:?}, force_bind={}",
            req.client_id, req.mobiles, req.force_bind
        );

        if let Some(scenario) = self.load_mock_scenario() {
            if let Some(bind_data) = scenario.get("bind_phones") {
                let resp: PhoneBindResponse = serde_json::from_value(bind_data.clone())
                    .unwrap_or_else(|e| {
                        eprintln!("[http-mock] 解析 bind_phones 场景失败: {}", e);
                        PhoneBindResponse { task_items: Some(Vec::new()), conflicts: Vec::new() }
                    });
                eprintln!(
                    "[http-mock] bind_phones 场景响应: task_items={}, conflicts={}",
                    resp.task_items.as_ref().map(|items| items.len()).unwrap_or(0),
                    resp.conflicts.len()
                );
                return Ok(resp);
            }
        }

        let items = self.batch_items_for_mobiles(&req.mobiles);
        let task_items = items.into_iter().map(|item| item.task_id).collect();
        Ok(PhoneBindResponse { task_items: Some(task_items), conflicts: Vec::new() })
    }

    async fn batch_fetch_tasks(
        &self,
        req: &BatchTasksRequest,
    ) -> Result<Vec<BatchTaskItem>, String> {
        eprintln!("[http-mock] batch_fetch_tasks: task_ids={:?}", req.task_ids);

        let mut items = self.batch_items_for_all();
        if !req.task_ids.is_empty() {
            items.retain(|item| req.task_ids.iter().any(|id| id == &item.task_id));
        }
        Ok(items)
    }

    async fn report_progress(&self, req: &ProgressReportRequest) -> Result<ApiResponse, String> {
        eprintln!(
            "[http-mock] report_progress: task={}, city={}, kw={}, round={}",
            req.task_id, req.city_name, req.keyword, req.round_no
        );
        Ok(ApiResponse::default())
    }

    async fn unbind_phones(&self, req: &UnbindPhonesRequest) -> Result<ApiResponse, String> {
        eprintln!(
            "[http-mock] unbind_phones: client={}, mobiles={}",
            req.client_id,
            req.mobiles.len()
        );
        Ok(ApiResponse::default())
    }
}

impl MockApiClient {
    fn batch_items_for_mobiles(&self, mobiles: &[String]) -> Vec<BatchTaskItem> {
        if mobiles.is_empty() {
            return Vec::new();
        }

        if let Some(scenario) = self.load_mock_scenario() {
            if let Some(pt) = scenario.get("phone_tasks") {
                if let Ok(phone_tasks) = serde_json::from_value::<
                    std::collections::HashMap<String, Vec<TaskDef>>,
                >(pt.clone())
                {
                    return phone_tasks
                        .into_iter()
                        .filter(|(mobile, _)| mobiles.contains(mobile))
                        .flat_map(|(mobile, defs)| {
                            defs.into_iter()
                                .map(move |def| task_def_to_batch_item(def, mobile.clone()))
                        })
                        .collect();
                }
            }
        }

        use crate::task_provider::load_mock_definitions;
        load_mock_definitions()
            .into_iter()
            .enumerate()
            .map(|(i, def)| {
                let mobile = mobiles[i % mobiles.len()].clone();
                task_def_to_batch_item(def, mobile)
            })
            .collect()
    }

    fn batch_items_for_all(&self) -> Vec<BatchTaskItem> {
        if let Some(scenario) = self.load_mock_scenario() {
            if let Some(pt) = scenario.get("phone_tasks") {
                if let Ok(phone_tasks) = serde_json::from_value::<
                    std::collections::HashMap<String, Vec<TaskDef>>,
                >(pt.clone())
                {
                    return phone_tasks
                        .into_iter()
                        .flat_map(|(mobile, defs)| {
                            defs.into_iter()
                                .map(move |def| task_def_to_batch_item(def, mobile.clone()))
                        })
                        .collect();
                }
            }
        }

        use crate::task_provider::load_mock_definitions;
        load_mock_definitions()
            .into_iter()
            .map(|def| task_def_to_batch_item(def, String::new()))
            .collect()
    }
}

fn task_def_to_batch_item(def: TaskDef, mobile: String) -> BatchTaskItem {
    BatchTaskItem {
        task_id: def.id,
        task_name: def.name,
        interval_minute: def.interval_minute,
        mobile,
        city_items: def
            .cities
            .into_iter()
            .map(|city| BatchTaskCityItem {
                city_name: city.name,
                point_name: city.poi,
                keywords: city.keywords,
            })
            .collect(),
    }
}
