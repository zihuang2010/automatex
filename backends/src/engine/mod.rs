mod handlers;
mod lifecycle;
mod tick;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::Emitter;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::constants::{keyword_status, task_status};
use crate::http;
use crate::storage::{Database, DeviceRow};
use crate::task_provider::{self, Task};

// ─── 运行中任务的状态 ──────────────────────────────────────────

/// 回退任务中所有 RUN 状态的关键词为 PENDING
pub(crate) fn rollback_running_keywords(task: &mut Task) {
    for city in &mut task.cities {
        for kw in &mut city.keywords {
            if kw.status == keyword_status::RUN {
                kw.status = keyword_status::PENDING.to_string();
            }
        }
    }
}

pub(crate) struct RunningTask {
    pub cancel: CancellationToken,
    pub started_at: i64,
    pub round_id: i64,
}

// ─── 事件负载（推送给前端）─────────────────────────────────────

/// 零拷贝快照：序列化时直接引用 tasks，避免深度 clone
#[derive(Serialize)]
struct TaskSnapshotRef<'a> {
    tasks: &'a [Task],
}

// ─── 引擎核心 ─────────────────────────────────────────────────

/// FIX #13: 提取已分配设备集合计算为公共辅助函数，避免各处重复代码
pub(crate) fn compute_assigned_set(tasks: &[Task]) -> HashSet<&str> {
    tasks
        .iter()
        .filter(|t| t.status == task_status::EXECUTING)
        .filter_map(|t| t.assigned_device.as_deref())
        .collect()
}

/// 从预加载的设备列表中挑选就绪设备（不执行 DB 查询，避免在写锁内阻塞）
pub(crate) fn pick_ready_serial(devices: &[DeviceRow], tasks: &[Task]) -> Result<String, String> {
    let assigned = compute_assigned_set(tasks);

    devices
        .iter()
        .find(|d| {
            d.state == crate::constants::device_state::DEVICE
                && !d.is_flagged
                && !assigned.contains(d.serial.as_str())
        })
        .map(|d| d.serial.clone())
        .ok_or_else(|| "当前没有就绪安全的设备，请检查设备状态".to_string())
}

pub struct TaskEngine {
    pub(crate) storage: Arc<Database>,
    pub(crate) http: Arc<dyn http::ApiClient>,
    pub(crate) tasks: RwLock<Vec<Task>>,
    pub(crate) running: RwLock<HashMap<String, RunningTask>>,
    /// 防重入：正在 reload 的 task_id 集合
    pub(crate) reloading: Mutex<HashSet<String>>,
    pub(crate) app_handle: tauri::AppHandle,
    /// emit_update 节流时间戳
    pub(crate) last_emit: Mutex<std::time::Instant>,
}

impl TaskEngine {
    pub async fn new(
        storage: Arc<Database>,
        http: Arc<dyn http::ApiClient>,
        app_handle: tauri::AppHandle,
    ) -> Arc<Self> {
        let tasks = task_provider::load_tasks(&storage).await;
        Arc::new(Self {
            storage,
            http,
            tasks: RwLock::new(tasks),
            running: RwLock::new(HashMap::new()),
            reloading: Mutex::new(HashSet::new()),
            app_handle,
            last_emit: Mutex::new(std::time::Instant::now() - Duration::from_secs(1)),
        })
    }

    /// 获取任务列表快照
    pub async fn get_tasks(&self) -> Vec<Task> {
        self.tasks.read().await.clone()
    }

    /// 重新从 DB 加载任务列表（保留仍在运行中的任务的运行时状态）
    pub async fn reload_tasks(&self) {
        let running_snapshot: std::collections::HashMap<String, (String, Option<String>)> = {
            let running = self.running.read().await;
            let tasks = self.tasks.read().await;
            tasks
                .iter()
                .filter(|t| running.contains_key(&t.id))
                .map(|t| (t.id.clone(), (t.status.clone(), t.assigned_device.clone())))
                .collect()
        };

        let mut tasks = task_provider::load_tasks(&self.storage).await;

        for task in &mut tasks {
            if let Some((status, device)) = running_snapshot.get(&task.id) {
                task.status = status.clone();
                task.assigned_device = device.clone();
            }
        }

        *self.tasks.write().await = tasks;
    }

    /// 推送任务状态到前端（节流控制）
    /// FIX #15: 直接在读锁内序列化引用，避免深度 clone 整个 Vec<Task>
    pub async fn emit_update(&self) {
        let throttle_ms = crate::constants::debug::EMIT_THROTTLE_MS;
        {
            let mut last = self.last_emit.lock().await;
            if last.elapsed() < Duration::from_millis(throttle_ms) {
                return;
            }
            *last = std::time::Instant::now();
        }
        let tasks = self.tasks.read().await;
        let snapshot = TaskSnapshotRef { tasks: &*tasks };
        let _ = self.app_handle.emit(crate::constants::tauri_event::TASK_UPDATE, &snapshot);
    }

    /// 强制推送任务状态（跳过节流，用于关键操作如同步完成后）
    pub async fn force_emit_update(&self) {
        {
            let mut last = self.last_emit.lock().await;
            *last = std::time::Instant::now();
        }
        let tasks = self.tasks.read().await;
        let snapshot = TaskSnapshotRef { tasks: &*tasks };
        let _ = self.app_handle.emit(crate::constants::tauri_event::TASK_UPDATE, &snapshot);
    }

    /// 重排城市顺序（仅 pending 城市）
    pub async fn reorder_cities(
        &self,
        task_id: &str,
        new_order: Vec<String>,
    ) -> Result<(), String> {
        {
            let mut tasks = self.tasks.write().await;
            let task = tasks.iter_mut().find(|t| t.id == task_id).ok_or("任务不存在")?;

            let (fixed, mut pending): (Vec<_>, Vec<_>) = task
                .cities
                .drain(..)
                .partition(|c| c.status != crate::constants::city_status::PENDING);

            pending.sort_by_key(|c| {
                new_order.iter().position(|name| name == &c.name).unwrap_or(usize::MAX)
            });

            task.cities = fixed.into_iter().chain(pending).collect();
        }

        self.storage.save_city_order(task_id, &new_order).await;
        self.emit_update().await;
        Ok(())
    }
}
