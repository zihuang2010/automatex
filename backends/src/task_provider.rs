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

// ─── 任务定义格式（Mock / HTTP 共用）───────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskDef {
    pub id: String,
    pub name: String,
    pub cities: Vec<CityDef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CityDef {
    pub name: String,
    pub poi: String,
    pub keywords: Vec<String>,
}

// ─── 任务提供者 ─────────────────────────────────────────────────

/// 同步任务缓存到数据库（仅在启动时调用一次）
pub async fn sync_task_cache(db: &Database) {
    let defs: Vec<TaskDef> = load_mock_definitions();
    for def in defs {
        let payload = serde_json::to_string(&def.cities).unwrap_or_default();
        db.upsert_task_cache(&def.id, &def.name, &payload, 1).await;
    }
}

/// 从 DB 缓存加载任务定义，合并进度，返回 Task 列表
/// 优先从e a_task_cache 读取，若 DB 为空则 fallback 到 mock
pub async fn load_tasks(db: &Database) -> Vec<Task> {
    let cached = db.load_all_task_defs().await;
    let defs: Vec<TaskDef> = if cached.is_empty() {
        // DB 无缓存，fallback 为 mock
        load_mock_definitions()
    } else {
        cached
            .into_iter()
            .filter_map(|(id, name, payload)| {
                let cities: Vec<CityDef> = serde_json::from_str(&payload).ok()?;
                Some(TaskDef { id, name, cities })
            })
            .collect()
    };
    let mut tasks = Vec::new();
    for def in defs {
        tasks.push(build_task(db, def).await);
    }
    tasks
}

/// 加载单个任务（优先 DB 缓存，fallback mock）
pub async fn load_task_by_id(db: &Database, target_id: &str) -> Option<Task> {
    // 优先从 DB 读取
    if let Some((id, name, payload)) = db.load_task_def_by_id(target_id).await {
        if let Ok(cities) = serde_json::from_str::<Vec<CityDef>>(&payload) {
            return Some(build_task(db, TaskDef { id, name, cities }).await);
        }
    }
    // Fallback: mock
    let defs: Vec<TaskDef> = load_mock_definitions();
    match defs.into_iter().find(|d| d.id == target_id) {
        Some(def) => Some(build_task(db, def).await),
        None => None,
    }
}

/// Mock 模式下按 ID 查找任务定义（供 TaskEngine 增量合并使用）
///
/// 优先从 `~/.automatex/mock_tasks_override.json` 读取（每次读磁盘，不缓存），
/// 这样修改外部文件不会触发 dev 重编译。找不到则 fallback 到内嵌的默认值。
pub fn load_mock_task_def_by_id(task_id: &str) -> Option<TaskDef> {
    // 尝试从运行时外部文件读取
    if let Some(def) = load_override_task_def(task_id) {
        eprintln!("[task_provider] 从外部 override 文件加载任务定义: {}", task_id);
        return Some(def);
    }
    // Fallback: 内嵌的默认定义
    load_mock_definitions().into_iter().find(|d| d.id == task_id)
}

/// 从 ~/.automatex/mock_tasks_override.json 读取指定 task 的定义
/// 每次调用都重新读磁盘（不缓存），方便测试时随时修改
fn load_override_task_def(task_id: &str) -> Option<TaskDef> {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    let path = std::path::Path::new(&home).join(".automatex").join("mock_tasks_override.json");
    if !path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    let defs: Vec<TaskDef> = serde_json::from_str(&content).ok()?;
    defs.into_iter().find(|d| d.id == task_id)
}

/// 内部：从 Mock 定义 + DB 进度 + DB 状态 构建单个 Task
pub async fn build_task(db: &Database, def: TaskDef) -> Task {
    // 加载已完成记录
    let progress = db.load_task_progress(&def.id).await;
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
    // 按用户自定义顺序重排 pending 城市
    if let Some(order) = db.load_city_order(&def.id).await {
        let (done, mut pending): (Vec<_>, Vec<_>) =
            cities.into_iter().partition(|c| c.status == city_status::DONE);

        pending
            .sort_by_key(|c| order.iter().position(|name| name == &c.name).unwrap_or(usize::MAX));

        cities = done.into_iter().chain(pending).collect();
    }

    // 默认状态：从进度推断
    let inferred_status = if all_done && !completed.is_empty() {
        task_status::SUCCESS
    } else if !completed.is_empty() {
        task_status::PAUSED
    } else {
        task_status::WAITING
    };

    // 从 DB 加载保存的运行时状态（覆盖推断值）
    let final_status = match db.load_task_state(&def.id).await {
        Some((saved_status, _)) => {
            if saved_status == task_status::EXECUTING {
                task_status::PAUSED.to_string()
            } else if saved_status == task_status::SUCCESS
                && inferred_status != task_status::SUCCESS
            {
                task_status::WAITING.to_string()
            } else if saved_status == task_status::WAITING
                && inferred_status != task_status::WAITING
            {
                inferred_status.to_string()
            } else {
                saved_status
            }
        },
        None => inferred_status.to_string(),
    };

    // 只有任务在 EXECUTING 或 PAUSED 时，才激活第一个 pending 城市
    if final_status != task_status::WAITING && final_status != task_status::SUCCESS {
        if let Some(first_pending) = cities.iter_mut().find(|c| c.status == city_status::PENDING) {
            first_pending.status = city_status::ACTIVE.to_string();
        }
    }

    let assigned_device: Option<String> = None;

    Task { id: def.id, name: def.name, status: final_status, assigned_device, cities }
}

/// 从嵌入资源读取 Mock 任务定义（OnceLock 缓存，只解析一次）
pub fn load_mock_definitions() -> Vec<TaskDef> {
    use std::sync::OnceLock;
    static DEFS: OnceLock<Vec<TaskDef>> = OnceLock::new();
    DEFS.get_or_init(|| {
        let json = include_str!("../resources/mock_tasks.json");
        serde_json::from_str(json).unwrap_or_default()
    })
    .clone()
}
