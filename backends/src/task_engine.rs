use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::Emitter;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::constants::{city_status, keyword_status, run_status, task_status};
use crate::storage::{Database, DeviceRow};
use crate::task_provider::{self, Task};
use rand::RngExt;

// ─── 运行中任务的状态 ──────────────────────────────────────────

struct RunningTask {
    cancel: CancellationToken,
    started_at: i64,
}

// ─── 事件负载（推送给前端）─────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct TaskSnapshot {
    pub tasks: Vec<Task>,
}

// ─── tick 执行后需要持久化的操作 ──────────────────────────────

/// tick 产生的副作用，在释放 tasks 写锁后执行
enum TickEffect {
    /// 推进了一个关键词
    KeywordDone { task_id: String, city_name: String, kw_name: String, device_serial: String },
    /// 整个任务执行完成
    TaskSuccess { task_id: String },
    /// 设备离线，任务需要标记 ERROR
    DeviceOffline { task_id: String },
    /// 风控触发：标记设备 + 任务 ERROR
    RiskControl { task_id: String, device_serial: String },
    /// 无需任何 DB 操作（城市切换、RUN→OK 等）
    None,
}

// ─── 引擎核心 ─────────────────────────────────────────────────

/// 从预加载的设备列表中挑选就绪设备（不执行 DB 查询，避免在写锁内阻塞）
fn pick_ready_serial(devices: &[DeviceRow], tasks: &[Task]) -> Result<String, String> {
    let assigned: HashSet<&str> = tasks
        .iter()
        .filter(|t| t.status == task_status::EXECUTING)
        .filter_map(|t| t.assigned_device.as_deref())
        .collect();

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
    storage: Arc<Database>,
    tasks: RwLock<Vec<Task>>,
    running: RwLock<HashMap<String, RunningTask>>,
    app_handle: tauri::AppHandle,
}

impl TaskEngine {
    pub fn new(storage: Arc<Database>, app_handle: tauri::AppHandle) -> Arc<Self> {
        let tasks = task_provider::load_tasks(&storage);
        Arc::new(Self {
            storage,
            tasks: RwLock::new(tasks),
            running: RwLock::new(HashMap::new()),
            app_handle,
        })
    }

    /// 获取任务列表快照
    pub async fn get_tasks(&self) -> Vec<Task> {
        self.tasks.read().await.clone()
    }

    /// 重新从 DB 加载任务列表
    #[allow(dead_code)]
    pub async fn reload_tasks(&self) {
        let db = Arc::clone(&self.storage);
        let tasks = tokio::task::spawn_blocking(move || task_provider::load_tasks(&db))
            .await
            .unwrap_or_default();
        *self.tasks.write().await = tasks;
    }

    // ─── 任务操作 ─────────────────────────────────────────────

    /// 启动任务
    pub async fn start_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        // DB 查询在写锁外执行，减少锁持有时间
        let devices = self.storage.load_all_devices();
        let serial = {
            let mut tasks = self.tasks.write().await;

            let task_status_val = tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.status.clone())
                .ok_or("任务不存在")?;
            if task_status_val != task_status::WAITING {
                return Err("任务状态非 WAITING，无法启动".into());
            }
            let serial = pick_ready_serial(&devices, &tasks)?;

            let task = tasks.iter_mut().find(|t| t.id == task_id).unwrap();
            task.status = task_status::EXECUTING.to_string();
            task.assigned_device = Some(serial.clone());
            serial
        };

        let db = Arc::clone(&self.storage);
        let tid = task_id.to_string();
        let ser = serial.clone();
        let started_at = tokio::task::spawn_blocking(move || {
            db.save_task_state(&tid, task_status::EXECUTING, Some(&ser));
            db.start_task_run(&tid, &ser)
        })
        .await
        .map_err(|e| format!("DB 操作失败: {}", e))?;

        self.spawn_loop(task_id, started_at).await;
        self.emit_update().await;
        Ok(())
    }

    /// 暂停任务
    /// FIX #5: 添加状态检查
    pub async fn pause_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        // FIX #5: 先检查任务状态
        {
            let tasks = self.tasks.read().await;
            let status = tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.status.clone())
                .ok_or("任务不存在")?;
            if status != task_status::EXECUTING {
                return Err(format!("任务状态为 {}，只有 EXECUTING 可以暂停", status));
            }
        }

        // FIX #3: 先 cancel 再 remove
        let run_info = {
            let mut running = self.running.write().await;
            if let Some(entry) = running.get(task_id) {
                entry.cancel.cancel();
            }
            running.remove(task_id)
        };

        if let Some(run) = run_info {
            let db = Arc::clone(&self.storage);
            let tid = task_id.to_string();
            let started_at = run.started_at;
            // FIX #4: 日志警告而非静默丢弃
            if let Err(e) = tokio::task::spawn_blocking(move || {
                db.finish_task_run(&tid, started_at, run_status::PAUSED);
            })
            .await
            {
                eprintln!("[engine] finish_task_run 失败: {}", e);
            }
        }

        {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                // RUN → PENDING（未执行完的关键词回退）
                for city in &mut task.cities {
                    for kw in &mut city.keywords {
                        if kw.status == keyword_status::RUN {
                            kw.status = keyword_status::PENDING.to_string();
                        }
                    }
                }
                task.status = task_status::PAUSED.to_string();
                task.assigned_device = None;
            }
        }

        let db = Arc::clone(&self.storage);
        let tid = task_id.to_string();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            db.save_task_state(&tid, task_status::PAUSED, None);
        })
        .await
        {
            eprintln!("[engine] save_task_state 失败: {}", e);
        }

        self.emit_update().await;
        Ok(())
    }

    /// 继续任务
    pub async fn resume_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let devices = self.storage.load_all_devices();
        let serial = {
            let mut tasks = self.tasks.write().await;

            let task_status_val = tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.status.clone())
                .ok_or("任务不存在")?;
            if task_status_val != task_status::PAUSED && task_status_val != task_status::ERROR {
                return Err(format!("任务状态为 {}，只有 PAUSED/ERROR 可以继续", task_status_val));
            }
            let serial = pick_ready_serial(&devices, &tasks)?;

            let task = tasks.iter_mut().find(|t| t.id == task_id).unwrap();
            task.status = task_status::EXECUTING.to_string();
            task.assigned_device = Some(serial.clone());
            serial
        };

        let db = Arc::clone(&self.storage);
        let tid = task_id.to_string();
        let ser = serial.clone();
        let started_at = tokio::task::spawn_blocking(move || {
            db.save_task_state(&tid, task_status::EXECUTING, Some(&ser));
            db.start_task_run(&tid, &ser)
        })
        .await
        .map_err(|e| format!("DB 操作失败: {}", e))?;

        self.spawn_loop(task_id, started_at).await;
        self.emit_update().await;
        Ok(())
    }

    /// 停止任务（清除进度，回到 WAITING）
    /// FIX #7: 添加任务存在性检查
    pub async fn stop_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        {
            let tasks = self.tasks.read().await;
            if !tasks.iter().any(|t| t.id == task_id) {
                return Err("任务不存在".into());
            }
        }

        // FIX #3: 先 cancel 再 remove
        let run_info = {
            let mut running = self.running.write().await;
            if let Some(entry) = running.get(task_id) {
                entry.cancel.cancel();
            }
            running.remove(task_id)
        };

        if let Some(run) = run_info {
            let db = Arc::clone(&self.storage);
            let tid = task_id.to_string();
            let started_at = run.started_at;
            if let Err(e) = tokio::task::spawn_blocking(move || {
                db.finish_task_run(&tid, started_at, run_status::STOPPED);
            })
            .await
            {
                eprintln!("[engine] finish_task_run 失败: {}", e);
            }
        }

        let db = Arc::clone(&self.storage);
        let tid = task_id.to_string();
        let fresh = tokio::task::spawn_blocking(move || {
            db.clear_task_progress(&tid);
            db.delete_task_state(&tid);
            task_provider::load_task_by_id(&db, &tid)
        })
        .await
        .map_err(|e| format!("DB 操作失败: {}", e))?;

        if let Some(fresh) = fresh {
            let mut tasks = self.tasks.write().await;
            if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
                tasks[pos] = fresh;
            }
        }

        self.emit_update().await;
        Ok(())
    }

    /// 重试任务（清除进度 + 重新启动）
    /// FIX #7: 添加任务存在性检查
    pub async fn retry_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        {
            let tasks = self.tasks.read().await;
            if !tasks.iter().any(|t| t.id == task_id) {
                return Err("任务不存在".into());
            }
        }

        // FIX #3: 先 cancel 再 remove
        let run_info = {
            let mut running = self.running.write().await;
            if let Some(entry) = running.get(task_id) {
                entry.cancel.cancel();
            }
            running.remove(task_id)
        };
        if let Some(run) = run_info {
            let db = Arc::clone(&self.storage);
            let tid = task_id.to_string();
            let started_at = run.started_at;
            if let Err(e) = tokio::task::spawn_blocking(move || {
                db.finish_task_run(&tid, started_at, run_status::STOPPED);
            })
            .await
            {
                eprintln!("[engine] finish_task_run 失败: {}", e);
            }
        }

        let db = Arc::clone(&self.storage);
        let tid = task_id.to_string();
        let fresh = tokio::task::spawn_blocking(move || {
            db.clear_task_progress(&tid);
            db.delete_task_state(&tid);
            task_provider::load_task_by_id(&db, &tid)
        })
        .await
        .map_err(|e| format!("DB 操作失败: {}", e))?;

        let devices = self.storage.load_all_devices();
        let serial = {
            let mut tasks = self.tasks.write().await;
            let serial = pick_ready_serial(&devices, &tasks)?;

            if let Some(mut fresh) = fresh {
                fresh.status = task_status::EXECUTING.to_string();
                fresh.assigned_device = Some(serial.clone());
                if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
                    tasks[pos] = fresh;
                }
            }
            serial
        };

        let db = Arc::clone(&self.storage);
        let tid = task_id.to_string();
        let ser = serial.clone();
        let started_at = tokio::task::spawn_blocking(move || {
            db.save_task_state(&tid, task_status::EXECUTING, Some(&ser));
            db.start_task_run(&tid, &ser)
        })
        .await
        .map_err(|e| format!("DB 操作失败: {}", e))?;

        self.spawn_loop(task_id, started_at).await;
        self.emit_update().await;
        Ok(())
    }

    /// 释放离线设备上的任务
    pub async fn release_offline_devices(&self, online_serials: &[String]) -> u32 {
        let online_set: HashSet<&str> = online_serials.iter().map(|s| s.as_str()).collect();
        let mut released = 0u32;

        let to_release: Vec<(String, Option<RunningTask>)> = {
            let tasks = self.tasks.read().await;
            let mut running = self.running.write().await;
            tasks
                .iter()
                .filter(|t| {
                    t.assigned_device.is_some()
                        && (t.status == task_status::EXECUTING || t.status == task_status::PAUSED)
                        && !online_set.contains(t.assigned_device.as_deref().unwrap_or(""))
                })
                .map(|t| {
                    // FIX #3: 先 cancel 再 remove
                    if let Some(entry) = running.get(&t.id) {
                        entry.cancel.cancel();
                    }
                    (t.id.clone(), running.remove(&t.id))
                })
                .collect()
        };

        for (task_id, run_info) in to_release {
            if let Some(run) = run_info {
                let db = Arc::clone(&self.storage);
                let tid = task_id.clone();
                let started_at = run.started_at;
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    db.finish_task_run(&tid, started_at, run_status::STOPPED);
                })
                .await
                {
                    eprintln!("[engine] finish_task_run 失败: {}", e);
                }
            }

            {
                let mut tasks = self.tasks.write().await;
                if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                    task.status = task_status::ERROR.to_string();
                    task.assigned_device = None;
                }
            }

            let db = Arc::clone(&self.storage);
            let tid = task_id.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                db.save_task_state(&tid, task_status::ERROR, None);
            })
            .await
            {
                eprintln!("[engine] save_task_state 失败: {}", e);
            }

            released += 1;
        }

        if released > 0 {
            self.emit_update().await;
        }
        released
    }

    /// 获取就绪设备列表（在线且未被任务占用）
    pub async fn get_ready_serials(&self) -> Vec<String> {
        let devices = self.storage.load_all_devices();
        let tasks = self.tasks.read().await;
        let assigned: HashSet<String> = tasks
            .iter()
            .filter(|t| t.status == task_status::EXECUTING)
            .filter_map(|t| t.assigned_device.clone())
            .collect();

        devices
            .into_iter()
            .filter(|d| {
                d.state == crate::constants::device_state::DEVICE
                    && !d.is_flagged
                    && !assigned.contains(&d.serial)
            })
            .map(|d| d.serial)
            .collect()
    }

    // ─── 内部方法 ─────────────────────────────────────────────

    async fn spawn_loop(self: &Arc<Self>, task_id: &str, started_at: i64) {
        let cancel = CancellationToken::new();
        self.running
            .write()
            .await
            .insert(task_id.to_string(), RunningTask { cancel: cancel.clone(), started_at });

        let engine = Arc::clone(self);
        let tid = task_id.to_string();

        tokio::spawn(async move {
            let completed = loop {
                tokio::select! {
                    _ = cancel.cancelled() => break false,
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {
                        let done = engine.tick(&tid).await;
                        engine.emit_update().await;
                        if done { break true; }
                    }
                }
            };

            // FIX #6: 安全处理 RunningTask 已被移走的情况
            let run_info = engine.running.write().await.remove(&tid);
            if completed {
                // 任务自然完成：需要记录 COMPLETED
                // 兜底：即使被移走也用原始 started_at
                let sa = run_info.map(|r| r.started_at).unwrap_or(started_at);
                let db = Arc::clone(&engine.storage);
                let task_id = tid.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    db.finish_task_run(&task_id, sa, run_status::COMPLETED);
                })
                .await
                {
                    eprintln!("[engine] finish_task_run(COMPLETED) 失败: {}", e);
                }
            }
            // cancelled: pause/stop 已经调用了 finish_task_run，无需重复
        });
    }

    /// 核心 tick 逻辑
    /// FIX #1: done 计数移到 RUN→OK 时
    /// FIX #2: 所有 TaskSuccess 都走 effect 通道（labeled block）
    async fn tick(&self, task_id: &str) -> bool {
        let effect = 'effect: {
            let mut tasks = self.tasks.write().await;
            let task = match tasks.iter_mut().find(|t| t.id == task_id) {
                Some(t) => t,
                None => break 'effect TickEffect::None,
            };

            if task.status != task_status::EXECUTING {
                break 'effect TickEffect::None;
            }

            let device_serial = task.assigned_device.clone().unwrap_or_default();

            // 检测设备是否仍在线，避免产生虚假进度
            let device_online = self
                .storage
                .get_device_by_serial(&device_serial)
                .map(|d| d.state == crate::constants::device_state::DEVICE)
                .unwrap_or(false);
            if !device_online {
                // 回退 RUN 状态的关键词
                for city in &mut task.cities {
                    for kw in &mut city.keywords {
                        if kw.status == keyword_status::RUN {
                            kw.status = keyword_status::PENDING.to_string();
                        }
                    }
                }
                task.status = task_status::ERROR.to_string();
                task.assigned_device = None;
                break 'effect TickEffect::DeviceOffline { task_id: task_id.to_string() };
            }

            // ── 风控模拟 ──────────────────────────────────────────
            // TODO: 这里未来替换为真实的 ADB 操作检测逻辑
            // 当前以 50% 概率随机触发风控，模拟操作设备时发现无法
            // 找到元素、应用崩溃等不可控异常
            {
                let mut rng = rand::rng();
                if rng.random_bool(0.5) {
                    eprintln!(
                        "[engine] risk-control triggered (simulated): task={}, device={}",
                        task_id, device_serial
                    );
                    // 回退 RUN 状态的关键词
                    for city in &mut task.cities {
                        for kw in &mut city.keywords {
                            if kw.status == keyword_status::RUN {
                                kw.status = keyword_status::PENDING.to_string();
                            }
                        }
                    }
                    task.status = task_status::ERROR.to_string();
                    task.assigned_device = None;
                    break 'effect TickEffect::RiskControl {
                        task_id: task_id.to_string(),
                        device_serial,
                    };
                }
            }

            // 找活跃城市或激活第一个 pending
            let active_idx =
                task.cities.iter().position(|c| c.status == city_status::ACTIVE).or_else(|| {
                    let idx = task.cities.iter().position(|c| c.status == city_status::PENDING)?;
                    task.cities[idx].status = city_status::ACTIVE.to_string();
                    Some(idx)
                });

            let Some(active_idx) = active_idx else {
                // FIX #2: 所有城市完成 → 走正常 effect 通道
                task.status = task_status::SUCCESS.to_string();
                task.assigned_device = None;
                break 'effect TickEffect::TaskSuccess { task_id: task_id.to_string() };
            };

            let city = &mut task.cities[active_idx];

            // FIX #1: 上一 tick 的 RUN → OK 时才 done += 1
            for kw in city.keywords.iter_mut() {
                if kw.status == keyword_status::RUN {
                    kw.status = keyword_status::OK.to_string();
                    city.done += 1;
                    city.progress = if city.total > 0 {
                        ((city.done as f64 / city.total as f64) * 100.0).round() as i32
                    } else {
                        0
                    };
                }
            }

            // 找下一个 pending 关键词
            let next_idx = city.keywords.iter().position(|k| k.status == keyword_status::PENDING);

            if let Some(idx) = next_idx {
                city.keywords[idx].status = keyword_status::RUN.to_string();

                TickEffect::KeywordDone {
                    task_id: task_id.to_string(),
                    city_name: city.name.clone(),
                    kw_name: city.keywords[idx].name.clone(),
                    device_serial,
                }
            } else {
                // 当前城市所有关键词完成
                city.status = city_status::DONE.to_string();
                city.progress = 100;

                // 激活下一个城市
                let next = task.cities.iter_mut().find(|c| c.status == city_status::PENDING);
                if let Some(nc) = next {
                    nc.status = city_status::ACTIVE.to_string();
                    TickEffect::None
                } else {
                    // 所有城市完成
                    task.status = task_status::SUCCESS.to_string();
                    task.assigned_device = None;
                    TickEffect::TaskSuccess { task_id: task_id.to_string() }
                }
            }
        }; // ← 写锁释放

        // Step 2: 锁外执行 DB 操作
        match effect {
            TickEffect::KeywordDone { task_id, city_name, kw_name, device_serial } => {
                let db = Arc::clone(&self.storage);
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    db.record_keyword_done(&task_id, &city_name, &kw_name, &device_serial);
                })
                .await
                {
                    eprintln!("[engine] record_keyword_done 失败: {}", e);
                }
                false
            },
            TickEffect::TaskSuccess { task_id } => {
                let db = Arc::clone(&self.storage);
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    db.save_task_state(&task_id, task_status::SUCCESS, None);
                })
                .await
                {
                    eprintln!("[engine] save_task_state(SUCCESS) 失败: {}", e);
                }
                true
            },
            TickEffect::DeviceOffline { task_id } => {
                let db = Arc::clone(&self.storage);
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    db.save_task_state(&task_id, task_status::ERROR, None);
                })
                .await
                {
                    eprintln!("[engine] save_task_state(ERROR/DeviceOffline) 失败: {}", e);
                }
                true // 返回 true 停止执行循环
            },
            TickEffect::RiskControl { task_id, device_serial } => {
                let db = Arc::clone(&self.storage);
                let app = self.app_handle.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    db.save_task_state(&task_id, task_status::ERROR, None);
                    db.flag_device(&device_serial);
                    // 通知前端：风控触发
                    let _ = app.emit(
                        "risk-control",
                        serde_json::json!({
                            "task_id": task_id,
                            "device_serial": device_serial,
                            "message": "设备风控触发，任务已停止，设备已标记"
                        }),
                    );
                    let _ = app.emit("devices-changed", ());
                })
                .await
                {
                    eprintln!("[engine] RiskControl 处理失败: {}", e);
                }
                true // 停止执行循环
            },
            TickEffect::None => false,
        }
    }

    /// 推送任务状态到前端
    async fn emit_update(&self) {
        let tasks = self.tasks.read().await;
        let snapshot = TaskSnapshot { tasks: tasks.clone() };
        let _ = self.app_handle.emit("task://update", &snapshot);
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

            // 分离：固定部分（done/active）和可排序部分（pending）
            let (fixed, mut pending): (Vec<_>, Vec<_>) = task
                .cities
                .drain(..)
                .partition(|c| c.status != crate::constants::city_status::PENDING);

            // 按 new_order 排序 pending
            pending.sort_by_key(|c| {
                new_order.iter().position(|name| name == &c.name).unwrap_or(usize::MAX)
            });

            // 合并写回
            task.cities = fixed.into_iter().chain(pending).collect();
        }

        // 落盘
        let db = Arc::clone(&self.storage);
        let order = new_order.clone();
        let tid = task_id.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            db.save_city_order(&tid, &order);
        })
        .await;

        // 推送更新
        self.emit_update().await;
        Ok(())
    }
}
