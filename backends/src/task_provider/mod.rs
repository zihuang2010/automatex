//! 任务提供者模块
//!
//! - `types.rs` — Task、TaskDef 等数据结构
//! - `mod.rs` — DB 加载、构建函数

pub mod types;

pub use types::*;

use std::collections::HashSet;

use crate::constants::{city_status, keyword_status, task_status};
use crate::storage::{Database, TaskStateRow};

/// 从 DB 缓存加载任务定义，合并当前轮次进度，返回 Task 列表
pub async fn load_tasks(db: &Database) -> Vec<Task> {
    let cached = db.load_all_task_defs().await;
    let mut defs = Vec::new();
    let mut all_orders: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (id, name, payload, city_order) in cached {
        let def = match parse_task_def_payload(&id, &name, &payload) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(co) = city_order {
            if let Ok(order) = serde_json::from_str::<Vec<String>>(&co) {
                all_orders.insert(id.clone(), order);
            }
        }
        defs.push(def);
    }

    // 批量预加载所有数据
    let all_states = db.load_all_task_states().await;

    let round_ids: Vec<i64> =
        all_states.values().filter_map(|state| state.current_round_id).collect();
    let all_progress = db.load_all_progress(&round_ids).await;
    let all_round_info = db.load_all_round_info(&round_ids).await;

    let mut progress_map: std::collections::HashMap<String, Vec<crate::storage::ProgressRow>> =
        std::collections::HashMap::new();
    for p in all_progress {
        progress_map.entry(p.task_id.clone()).or_default().push(p);
    }

    let mut tasks = Vec::new();
    for def in defs {
        let progress = progress_map.remove(&def.id).unwrap_or_default();
        let state = all_states.get(&def.id).cloned();
        let order = all_orders.get(&def.id).cloned();
        let round_id = state.as_ref().and_then(|state| state.current_round_id);
        let round_no = round_id.and_then(|rid| all_round_info.get(&rid).copied()).unwrap_or(0);
        tasks.push(build_task_batched(def, progress, state, order, round_no));
    }
    tasks
}

/// 从 DB 加载单个任务
pub async fn load_task_by_id(db: &Database, target_id: &str) -> Option<Task> {
    let (id, name, payload) = db.load_task_def_by_id(target_id).await?;
    let def = parse_task_def_payload(&id, &name, &payload).ok()?;
    Some(build_task(db, def).await)
}

fn parse_task_def_payload(
    id: &str,
    name: &str,
    payload: &str,
) -> Result<TaskDef, serde_json::Error> {
    if let Ok(def) = serde_json::from_str::<TaskDef>(payload) {
        return Ok(TaskDef {
            id: if def.id.is_empty() { id.to_string() } else { def.id },
            name: if def.name.is_empty() { name.to_string() } else { def.name },
            interval_minute: def.interval_minute,
            cities: def.cities,
        });
    }

    let cities = serde_json::from_str::<Vec<CityDef>>(payload)?;
    Ok(TaskDef { id: id.to_string(), name: name.to_string(), interval_minute: None, cities })
}

pub fn summarize_task(task: &Task) -> TaskSummary {
    let keyword_total: i32 = task.cities.iter().map(|city| city.total).sum();
    let keyword_done: i32 = task.cities.iter().map(|city| city.done).sum();
    let progress = if keyword_total > 0 {
        ((keyword_done as f64 / keyword_total as f64) * 100.0).round() as i32
    } else {
        0
    };

    let active_city_name = task
        .cities
        .iter()
        .find(|city| city.status == city_status::ACTIVE)
        .map(|city| city.name.clone())
        .or_else(|| task.current_city_name.clone());
    let active_city = active_city_name
        .as_ref()
        .and_then(|name| task.cities.iter().find(|city| city.name == *name));
    let presentation_status =
        derive_presentation_status(&task.status, task.runtime_status.as_deref());

    TaskSummary {
        id: task.id.clone(),
        name: task.name.clone(),
        status: task.status.clone(),
        runtime_status: task.runtime_status.clone(),
        presentation_status,
        assigned_device: task.assigned_device.clone(),
        city_count: task.cities.len() as i32,
        keyword_total,
        keyword_done,
        progress,
        active_city_name,
        active_city_progress: active_city.map(|city| city.progress),
        active_city_done: active_city.map(|city| city.done),
        active_city_total: active_city.map(|city| city.total),
        current_city_name: task.current_city_name.clone(),
        current_keyword_name: task.current_keyword_name.clone(),
        interval_minute: task.interval_minute,
        round_no: task.round_no,
        next_round_at: task.next_round_at,
    }
}

pub fn derive_presentation_status(status: &str, runtime_status: Option<&str>) -> String {
    use crate::constants::task_presentation_status as presentation;

    match status {
        task_status::WAITING => presentation::READY.to_string(),
        task_status::EXECUTING => match runtime_status {
            Some("interval_waiting") => presentation::WAITING_NEXT_ROUND.to_string(),
            _ => presentation::RUNNING.to_string(),
        },
        task_status::PAUSED => match runtime_status {
            Some("interval_paused") => presentation::PAUSED_WAITING.to_string(),
            _ => presentation::PAUSED_MANUAL.to_string(),
        },
        task_status::ERROR => presentation::ERROR_PAUSED.to_string(),
        task_status::SUCCESS => presentation::COMPLETED.to_string(),
        _ => presentation::READY.to_string(),
    }
}

/// 内部：从任务定义 + DB 进度 + DB 状态 构建单个 Task
pub async fn build_task(db: &Database, def: TaskDef) -> Task {
    let state = db.load_task_state(&def.id).await;
    let round_id = state.as_ref().and_then(|state| state.current_round_id).unwrap_or(0);
    let progress = db.load_task_progress(&def.id, round_id).await;
    let order = db.load_city_order(&def.id).await;
    let round_no = if round_id > 0 { db.get_round_no(round_id).await.unwrap_or(0) } else { 0 };
    build_task_batched(def, progress, state, order, round_no)
}

/// 从预加载数据构建单个 Task（无 DB 访问）
fn build_task_batched(
    def: TaskDef,
    progress: Vec<crate::storage::ProgressRow>,
    saved_state: Option<TaskStateRow>,
    city_order: Option<Vec<String>>,
    round_no: i32,
) -> Task {
    let completed: HashSet<(String, String)> = progress
        .iter()
        .filter(|p| p.status == keyword_status::OK)
        .map(|p| (p.city_name.clone(), p.keyword_name.clone()))
        .collect();

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

        let progress = if total > 0 {
            ((done as f64 / total as f64) * 100.0).round() as i32
        } else {
            0
        };

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
    if let Some(order) = city_order {
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

    // 从保存的运行时状态（覆盖推断值）
    let (final_status, assigned_device) = match saved_state {
        Some(ref saved) => {
            let saved_status = &saved.status;
            let status = if saved_status == task_status::EXECUTING {
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
                saved_status.clone()
            };
            let device = if saved_status == task_status::EXECUTING {
                None
            } else {
                saved.assigned_device.clone()
            };
            (status, device)
        },
        None => (inferred_status.to_string(), None),
    };

    let current_city_name = saved_state.as_ref().and_then(|s| s.current_city_name.clone());
    let current_keyword_name = saved_state.as_ref().and_then(|s| s.current_keyword_name.clone());
    let saved_status = saved_state.as_ref().map(|s| s.status.as_str());
    let saved_runtime_status = saved_state.as_ref().and_then(|s| s.runtime_status.clone());
    let saved_next_round_at = saved_state.as_ref().and_then(|s| s.next_wakeup_at);

    // 只有任务在 EXECUTING 或 PAUSED 时，才激活当前或第一个 pending 城市
    if final_status != task_status::WAITING && final_status != task_status::SUCCESS {
        let mut activated = false;
        if let Some(ref city_name) = current_city_name {
            if let Some(city) = cities.iter_mut().find(|c| {
                c.name == *city_name
                    && c.status != city_status::DONE
                    && c.keywords.iter().any(|k| k.status != keyword_status::OK)
            }) {
                city.status = city_status::ACTIVE.to_string();
                if saved_status == Some(task_status::EXECUTING) {
                    if let Some(ref keyword_name) = current_keyword_name {
                        if let Some(keyword) = city.keywords.iter_mut().find(|k| {
                            k.name == *keyword_name && k.status == keyword_status::PENDING
                        }) {
                            keyword.status = keyword_status::RUN.to_string();
                        }
                    }
                }
                activated = true;
            }
        }

        if !activated {
            if let Some(first_pending) =
                cities.iter_mut().find(|c| c.status == city_status::PENDING)
            {
                first_pending.status = city_status::ACTIVE.to_string();
            }
        }
    }

    let current_round_id = saved_state.as_ref().and_then(|s| s.current_round_id);
    let runtime_status = match (saved_status, saved_runtime_status.as_deref()) {
        (Some(task_status::EXECUTING), Some("interval_waiting")) => {
            Some("interval_paused".to_string())
        },
        (_, runtime_status) => runtime_status.map(str::to_string),
    };
    let presentation_status = derive_presentation_status(&final_status, runtime_status.as_deref());

    Task {
        id: def.id,
        name: def.name,
        status: final_status,
        runtime_status,
        presentation_status,
        assigned_device,
        cities,
        interval_minute: def.interval_minute,
        round_no,
        current_round_id,
        current_city_name,
        current_keyword_name,
        next_round_at: saved_next_round_at,
    }
}
