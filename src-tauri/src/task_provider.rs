use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::constants::{city_status, keyword_status, task_status};
use crate::storage::Database;

// ─── 任务数据结构（前端交互用）────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskKeyword {
    pub name: String,
    pub status: String, // "pending" | "run" | "ok"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCity {
    pub name: String,
    pub poi: String,
    pub progress: i32, // 0-100
    pub total: i32,
    pub done: i32,
    pub status: String, // "pending" | "active" | "done"
    pub keywords: Vec<TaskKeyword>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub status: String, // "WAITING" | "EXECUTING" | "PAUSED" | "SUCCESS" | "ERROR"
    pub assigned_device: Option<String>,
    pub cities: Vec<TaskCity>,
}

// ─── Mock 定义格式 ──────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
struct MockTaskDef {
    id: String,
    name: String,
    cities: Vec<MockCityDef>,
}

#[derive(Debug, Deserialize, Serialize)]
struct MockCityDef {
    name: String,
    poi: String,
    keywords: Vec<String>,
}

// ─── 任务提供者 ─────────────────────────────────────────────────

/// 加载 Mock JSON 任务定义，合并 DB 中的已完成进度，返回恢复后的任务列表
pub fn load_tasks(db: &Database) -> Vec<Task> {
    let defs = load_mock_definitions();
    let mut tasks = Vec::new();

    for def in defs {
        // 从 DB 缓存并获取已完成进度
        let payload = serde_json::to_string(&def.cities).unwrap_or_default();
        db.upsert_task_cache(&def.id, &def.name, &payload, 1);

        // 加载已完成记录
        let progress = db.load_task_progress(&def.id);
        let completed: HashSet<(String, String)> = progress
            .iter()
            .filter(|p| p.status == keyword_status::OK)
            .map(|p| (p.city_name.clone(), p.keyword_name.clone()))
            .collect();

        // 合并定义 + 进度
        let mut cities = Vec::new();
        let mut all_done = true;

        for city_def in &def.cities {
            let mut keywords = Vec::new();
            let mut done = 0;
            let total = city_def.keywords.len() as i32;

            for kw_name in &city_def.keywords {
                let is_done = completed.contains(&(city_def.name.clone(), kw_name.clone()));
                if is_done {
                    done += 1;
                }
                keywords.push(TaskKeyword {
                    name: kw_name.clone(),
                    status: if is_done {
                        keyword_status::OK.to_string()
                    } else {
                        keyword_status::PENDING.to_string()
                    },
                });
            }

            let city_status_val = if done >= total {
                city_status::DONE.to_string()
            } else {
                all_done = false;
                city_status::PENDING.to_string()
            };

            let progress =
                if total > 0 { ((done as f64 / total as f64) * 100.0).round() as i32 } else { 0 };

            cities.push(TaskCity {
                name: city_def.name.clone(),
                poi: city_def.poi.clone(),
                progress,
                total,
                done,
                status: city_status_val,
                keywords,
            });
        }

        // 激活第一个未完成的城市
        if let Some(first_pending) = cities.iter_mut().find(|c| c.status == city_status::PENDING) {
            first_pending.status = city_status::ACTIVE.to_string();
        }

        // 任务状态：有进度但未全完成 → PAUSED（上次中断），全完成 → SUCCESS
        let task_status_val = if all_done && !completed.is_empty() {
            task_status::SUCCESS
        } else if !completed.is_empty() {
            task_status::PAUSED
        } else {
            task_status::WAITING
        };

        tasks.push(Task {
            id: def.id,
            name: def.name,
            status: task_status_val.to_string(),
            assigned_device: None,
            cities,
        });
    }

    tasks
}

/// 从嵌入资源读取 Mock 任务定义
fn load_mock_definitions() -> Vec<MockTaskDef> {
    let json = include_str!("../resources/mock_tasks.json");
    serde_json::from_str(json).unwrap_or_default()
}
