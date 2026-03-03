use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::Emitter;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use crate::http_client::HttpClient;

use crate::constants::{city_status, keyword_status, run_status, task_status};
use crate::storage::{Database, DeviceRow};
use crate::task_provider::{self, Task};
use rand::RngExt;

// ─── 运行中任务的状态 ──────────────────────────────────────────

/// 回退任务中所有 RUN 状态的关键词为 PENDING
fn rollback_running_keywords(task: &mut Task) {
    for city in &mut task.cities {
        for kw in &mut city.keywords {
            if kw.status == keyword_status::RUN {
                kw.status = keyword_status::PENDING.to_string();
            }
        }
    }
}

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
    http: Arc<HttpClient>,
    tasks: RwLock<Vec<Task>>,
    running: RwLock<HashMap<String, RunningTask>>,
    /// 防重入：正在 reload 的 task_id 集合
    reloading: Mutex<HashSet<String>>,
    app_handle: tauri::AppHandle,
}

impl TaskEngine {
    pub fn new(
        storage: Arc<Database>,
        http: Arc<HttpClient>,
        app_handle: tauri::AppHandle,
    ) -> Arc<Self> {
        let tasks = task_provider::load_tasks(&storage);
        Arc::new(Self {
            storage,
            http,
            tasks: RwLock::new(tasks),
            running: RwLock::new(HashMap::new()),
            reloading: Mutex::new(HashSet::new()),
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
                rollback_running_keywords(task);
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
            // FIX #4: 回滚上次残留的 RUN 关键词
            rollback_running_keywords(task);
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

    // ─── FIX #5: 提取公共 cancel + cleanup 方法 ───────────────
    /// 取消正在运行的任务循环，结束 run 记录，清进度，重建任务
    async fn cancel_and_cleanup(&self, task_id: &str) -> Result<Option<Task>, String> {
        {
            let tasks = self.tasks.read().await;
            if !tasks.iter().any(|t| t.id == task_id) {
                return Err("任务不存在".into());
            }
        }

        // cancel + remove
        let run_info = {
            let mut running = self.running.write().await;
            if let Some(entry) = running.get(task_id) {
                entry.cancel.cancel();
            }
            running.remove(task_id)
        };

        // 结束 run 记录 + 清进度 + 重建（合并到一次 spawn_blocking）
        let db = Arc::clone(&self.storage);
        let tid = task_id.to_string();
        let started = run_info.map(|r| r.started_at);
        let fresh = tokio::task::spawn_blocking(move || {
            if let Some(sa) = started {
                db.finish_task_run(&tid, sa, run_status::STOPPED);
            }
            db.clear_task_progress(&tid);
            db.delete_task_state(&tid);
            task_provider::load_task_by_id(&db, &tid)
        })
        .await
        .map_err(|e| format!("DB 操作失败: {}", e))?;

        Ok(fresh)
    }

    /// 停止任务（清除进度，回到 WAITING）
    pub async fn stop_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let fresh = self.cancel_and_cleanup(task_id).await?;

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
    pub async fn retry_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let fresh = self.cancel_and_cleanup(task_id).await?;

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

        // Step 1: 读锁收集需要释放的任务 ID（不持有 running 锁）
        let task_ids_to_release: Vec<String> = {
            let tasks = self.tasks.read().await;
            tasks
                .iter()
                .filter(|t| {
                    t.assigned_device.is_some()
                        && (t.status == task_status::EXECUTING || t.status == task_status::PAUSED)
                        && !online_set.contains(t.assigned_device.as_deref().unwrap_or(""))
                })
                .map(|t| t.id.clone())
                .collect()
        };

        // Step 2: 单独获取 running 写锁，cancel + remove
        let run_infos: Vec<(String, Option<RunningTask>)> = {
            let mut running = self.running.write().await;
            task_ids_to_release
                .into_iter()
                .map(|tid| {
                    if let Some(entry) = running.get(&tid) {
                        entry.cancel.cancel();
                    }
                    let info = running.remove(&tid);
                    (tid, info)
                })
                .collect()
        };

        // FIX #9: 合并到一次 spawn_blocking，避免循环内多次 spawn
        if !run_infos.is_empty() {
            let db = Arc::clone(&self.storage);
            let infos: Vec<(String, Option<i64>)> = run_infos
                .iter()
                .map(|(tid, ri)| (tid.clone(), ri.as_ref().map(|r| r.started_at)))
                .collect();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                for (tid, started) in &infos {
                    if let Some(sa) = started {
                        db.finish_task_run(tid, *sa, run_status::STOPPED);
                    }
                    db.save_task_state(tid, task_status::ERROR, None);
                }
            })
            .await
            {
                eprintln!("[engine] release_offline DB 批量操作失败: {}", e);
            }
            released = run_infos.len() as u32;
        }

        // Step 4: 批量更新 tasks 写锁（只获取一次）
        if released > 0 {
            {
                let mut tasks = self.tasks.write().await;
                for (task_id, _) in &run_infos {
                    if let Some(task) = tasks.iter_mut().find(|t| t.id == *task_id) {
                        task.status = task_status::ERROR.to_string();
                        task.assigned_device = None;
                    }
                }
            }
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
        // Step 1: DB 查询在写锁外执行（避免在持有 RwLock 时阻塞在同步 Mutex 上）
        let device_serial_for_check = {
            let tasks = self.tasks.read().await;
            tasks.iter().find(|t| t.id == task_id).and_then(|t| t.assigned_device.clone())
        };
        let device_online = device_serial_for_check
            .as_deref()
            .map(|serial| {
                self.storage
                    .get_device_by_serial(serial)
                    .map(|d| d.state == crate::constants::device_state::DEVICE)
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        // Step 2: 获取写锁执行状态变更
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

            // 使用锁外查询的结果
            if !device_online {
                rollback_running_keywords(task);
                task.status = task_status::ERROR.to_string();
                task.assigned_device = None;
                break 'effect TickEffect::DeviceOffline { task_id: task_id.to_string() };
            }

            // ── 风控模拟 ──────────────────────────────────────────
            // TODO: 这里未来替换为真实的 ADB 操作检测逻辑
            // 当前以 10% 概率随机触发风控，模拟操作设备时发现无法
            // 找到元素、应用崩溃等不可控异常
            {
                let mut rng = rand::rng();
                if rng.random_bool(0.05) {
                    eprintln!(
                        "[engine] risk-control triggered (simulated): task={}, device={}",
                        task_id, device_serial
                    );
                    // 回退 RUN 状态的关键词
                    rollback_running_keywords(task);
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
                // FIX #2: 二次确认设备在线状态，减少 TOCTOU 误判
                let still_offline = {
                    let tasks = self.tasks.read().await;
                    let serial = tasks
                        .iter()
                        .find(|t| t.id == task_id)
                        .and_then(|t| t.assigned_device.clone());
                    serial
                        .as_deref()
                        .map(|s| {
                            self.storage
                                .get_device_by_serial(s)
                                .map(|d| d.state != crate::constants::device_state::DEVICE)
                                .unwrap_or(true)
                        })
                        .unwrap_or(true)
                };
                if !still_offline {
                    eprintln!("[engine] 设备已恢复在线，跳过 DeviceOffline 标记: {}", task_id);
                    return false; // 不终止循环
                }
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
    // TODO #8: 性能优化 — 当前每次都 clone 全量任务列表，后续可用版本号 diff 或 Arc 包装减少内存分配
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

    // ─── MQTT 消息处理 ─────────────────────────────────────────

    /// 处理 MQTT 踢设备指令
    /// 根据 hw_serial 找到本地设备，暂停关联任务并删除设备
    pub async fn handle_device_kick(self: &Arc<Self>, hw_serials: Vec<String>) -> u32 {
        let mut kicked = 0u32;

        for hw_serial in &hw_serials {
            // 通过 hw_serial 找到本地设备的 serial
            let device = self.storage.get_device_by_hw_serial(hw_serial);
            let Some(device) = device else { continue };
            let serial = device.serial.clone();

            // 检查是否有任务绑定在这个设备上
            let task_to_pause: Option<String> = {
                let tasks = self.tasks.read().await;
                tasks
                    .iter()
                    .find(|t| {
                        t.assigned_device.as_deref() == Some(&serial)
                            && (t.status == task_status::EXECUTING
                                || t.status == task_status::PAUSED)
                    })
                    .map(|t| t.id.clone())
            };

            // 如果有任务在执行 → 暂停
            if let Some(task_id) = task_to_pause {
                if let Err(e) = self.pause_task(&task_id).await {
                    eprintln!("[engine] 踢设备时暂停任务失败: task={}, err={}", task_id, e);
                }
            }

            // 从 DB 删除设备
            self.storage.delete_device(&serial);
            eprintln!("[engine] 设备已踢下线: hw_serial={}, serial={}", hw_serial, serial);
            kicked += 1;
        }

        if kicked > 0 {
            let _ = self.app_handle.emit("devices-changed", ());
            self.emit_update().await;
        }
        kicked
    }

    /// 增量合并单个任务（reload_task 核心逻辑）
    ///
    /// FIX #1: was_success 判断移入写锁消除竞态
    /// FIX #3: 防重入由 handle_task_reload 的 reloading Mutex 保证
    /// FIX #7: payload 序列化失败时 log + return
    /// FIX #14: 统一走 http.fetch_task（mock/real 自动切换）
    async fn merge_single_task(&self, task_id: &str) {
        // ── Step 1: 通过 HttpClient 获取最新定义（mock/real 自动切换） ──
        let new_def = match self.http.fetch_task(task_id).await {
            Ok(def) => def,
            Err(e) => {
                eprintln!("[engine] merge_single_task: 获取任务定义失败 {}: {}", task_id, e);
                return;
            },
        };

        // ── Step 2: 更新 DB 缓存（FIX #7: 序列化失败则中止） ──
        let payload = match serde_json::to_string(&new_def.cities) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[engine] merge_single_task: 序列化 payload 失败: {}", e);
                return;
            },
        };
        let db = Arc::clone(&self.storage);
        let def_for_cache = new_def.clone();
        let tid = task_id.to_string();
        let payload_clone = payload;
        let _ = tokio::task::spawn_blocking(move || {
            db.upsert_task_cache(&tid, &def_for_cache.name, &payload_clone, 1);
        })
        .await;

        // ── Step 3: 在写锁内判断 was_success 并清进度（FIX #1: 消除竞态） ──
        // 不能在锁外判断再锁内操作，否则有 TOCTOU
        let was_success = {
            let tasks = self.tasks.read().await;
            tasks.iter().any(|t| t.id == task_id && t.status == task_status::SUCCESS)
        };
        if was_success {
            let db = Arc::clone(&self.storage);
            let tid = task_id.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                db.clear_task_progress(&tid);
                db.delete_task_state(&tid);
                eprintln!("[engine] 任务 {} 原状态 SUCCESS，已清空旧进度", tid);
            })
            .await;
        }

        // ── Step 4: 用 build_task 重建 ──
        let db = Arc::clone(&self.storage);
        let merged = tokio::task::spawn_blocking(move || task_provider::build_task(&db, new_def))
            .await
            .expect("build_task panic");

        // ── Step 5: 写锁内原子合并（FIX #1: 所有判断在同一写锁内） ──
        let mut tasks = self.tasks.write().await;
        if let Some(local) = tasks.iter_mut().find(|t| t.id == task_id) {
            let is_running = local.status == task_status::EXECUTING
                || local.status == task_status::PAUSED
                || local.status == task_status::ERROR;

            if is_running {
                let saved_status = local.status.clone();
                let saved_device = local.assigned_device.clone();
                *local = merged;
                local.status = saved_status;
                local.assigned_device = saved_device;
                eprintln!("[engine] merge_single_task: 任务 {} 执行中，增量合并完成", task_id);
            } else {
                *local = merged;
                eprintln!("[engine] merge_single_task: 任务 {} 未执行，全量覆盖", task_id);
            }
        } else {
            eprintln!("[engine] merge_single_task: 新任务 {} 已插入", task_id);
            tasks.push(merged);
        }

        // 收集合法的 (city_name, keyword_name) 组合
        let valid_pairs: Vec<(String, String)> = tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| {
                t.cities
                    .iter()
                    .flat_map(|c| c.keywords.iter().map(move |k| (c.name.clone(), k.name.clone())))
                    .collect()
            })
            .unwrap_or_default();
        drop(tasks); // 释放写锁

        // 清理孤儿进度记录
        let db = Arc::clone(&self.storage);
        let tid = task_id.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            db.cleanup_orphan_progress(&tid, &valid_pairs);
        })
        .await;
    }

    /// 处理 MQTT 任务数据变更通知
    pub async fn handle_task_reload(self: &Arc<Self>, action: &str, task_id: Option<&str>) {
        match action {
            "reload_all" => {
                eprintln!("[engine] 收到 reload_all，重新加载所有任务");
                self.reload_tasks().await;
                self.emit_update().await;
            },
            "reload_task" => {
                if let Some(tid) = task_id {
                    // FIX #3: 防重入 — 同一 task_id 不并发 reload
                    {
                        let mut guard = self.reloading.lock().await;
                        if guard.contains(tid) {
                            eprintln!("[engine] reload_task 防重入跳过: {}", tid);
                            return;
                        }
                        guard.insert(tid.to_string());
                    }
                    eprintln!("[engine] 收到 reload_task: {}", tid);
                    self.merge_single_task(tid).await;
                    self.emit_update().await;
                    // 释放防重入标记
                    self.reloading.lock().await.remove(tid);
                }
            },
            "delete_task" => {
                if let Some(tid) = task_id {
                    eprintln!("[engine] 收到 delete_task: {}", tid);
                    // 如果任务正在执行 → 先停止
                    let is_running = {
                        let tasks = self.tasks.read().await;
                        tasks.iter().any(|t| {
                            t.id == tid
                                && (t.status == task_status::EXECUTING
                                    || t.status == task_status::PAUSED)
                        })
                    };
                    if is_running {
                        let _ = self.stop_task(tid).await;
                    }
                    // 从内存中移除
                    {
                        let mut tasks = self.tasks.write().await;
                        tasks.retain(|t| t.id != tid);
                    }
                    // 从 DB 移除缓存
                    let db = Arc::clone(&self.storage);
                    let tid_owned = tid.to_string();
                    let _ = tokio::task::spawn_blocking(move || {
                        db.delete_task_cache(&tid_owned);
                    })
                    .await;
                    self.emit_update().await;
                }
            },
            _ => {
                eprintln!("[engine] 未知的 task reload action: {}", action);
            },
        }
    }
}
