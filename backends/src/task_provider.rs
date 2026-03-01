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

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MockTaskDef {
    id: String,
    name: String,
    cities: Vec<MockCityDef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MockCityDef {
    name: String,
    poi: String,
    keywords: Vec<String>,
}

// ─── 任务提供者 ─────────────────────────────────────────────────

/// 同步任务缓存到数据库（仅在启动时调用一次）
pub fn sync_task_cache(db: &Database) {
    let defs = load_mock_definitions();
    for def in defs {
        let payload = serde_json::to_string(&def.cities).unwrap_or_default();
        db.upsert_task_cache(&def.id, &def.name, &payload, 1);
    }
}

/// 加载 Mock JSON 任务定义，合并 DB 中的已完成进度，返回恢复后的任务列表（纯读操作）
pub fn load_tasks(db: &Database) -> Vec<Task> {
    let defs = load_mock_definitions();
    let mut tasks = Vec::new();

    for def in defs {
        tasks.push(build_task(db, def));
    }

    tasks
}

/// #4: 加载单个任务（避免全量加载再过滤）
pub fn load_task_by_id(db: &Database, target_id: &str) -> Option<Task> {
    let defs = load_mock_definitions();
    defs.into_iter().find(|d| d.id == target_id).map(|def| build_task(db, def))
}

/// 内部：从 Mock 定义 + DB 进度 + DB 状态 构建单个 Task
fn build_task(db: &Database, def: MockTaskDef) -> Task {
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
    // 按用户自定义顺序重排 pending 城市
    if let Some(order) = db.load_city_order(&def.id) {
        // 分离 done 和 pending 城市
        let (done, mut pending): (Vec<_>, Vec<_>) =
            cities.into_iter().partition(|c| c.status == city_status::DONE);

        // 按 order 排序 pending（未在 order 中的排最后）
        pending
            .sort_by_key(|c| order.iter().position(|name| name == &c.name).unwrap_or(usize::MAX));

        cities = done.into_iter().chain(pending).collect();
    }

    // 激活第一个未完成的城市
    if let Some(first_pending) = cities.iter_mut().find(|c| c.status == city_status::PENDING) {
        first_pending.status = city_status::ACTIVE.to_string();
    }

    // 默认状态：从进度推断
    let inferred_status = if all_done && !completed.is_empty() {
        task_status::SUCCESS
    } else if !completed.is_empty() {
        task_status::PAUSED
    } else {
        task_status::WAITING
    };

    // #1: 从 DB 加载保存的运行时状态（覆盖推断值）
    let final_status = match db.load_task_state(&def.id) {
        Some((saved_status, _)) => {
            // EXECUTING → PAUSED（重启后设备不再绑定）
            if saved_status == task_status::EXECUTING {
                task_status::PAUSED.to_string()
            } else if saved_status == task_status::WAITING
                && inferred_status != task_status::WAITING
            {
                // 如果 DB 记录 WAITING 但实际有进度，以推断为准
                inferred_status.to_string()
            } else {
                saved_status
            }
        },
        None => inferred_status.to_string(),
    };
    // 重启后统一释放设备绑定（与手动暂停/停止行为一致）
    let assigned_device: Option<String> = None;

    Task { id: def.id, name: def.name, status: final_status, assigned_device, cities }
}

/// 从嵌入资源读取 Mock 任务定义（OnceLock 缓存，只解析一次）
fn load_mock_definitions() -> Vec<MockTaskDef> {
    use std::sync::OnceLock;
    static DEFS: OnceLock<Vec<MockTaskDef>> = OnceLock::new();
    DEFS.get_or_init(|| {
        let json = include_str!("../resources/mock_tasks.json");
        serde_json::from_str(json).unwrap_or_default()
    })
    .clone()
}
