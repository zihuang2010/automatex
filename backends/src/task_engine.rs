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
    round_id: i64,
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
    KeywordDone {
        task_id: String,
        city_name: String,
        kw_name: String,
        device_serial: String,
    },
    /// 整个任务执行完成
    TaskSuccess { task_id: String },
    /// 设备离线，任务需要标记 ERROR
    DeviceOffline {
        task_id: String,
        device_serial: String,
    },
    /// 风控触发：标记设备 + 任务 ERROR
    RiskControl {
        task_id: String,
        device_serial: String,
    },
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
    /// emit_update 节流时间戳
    last_emit: Mutex<std::time::Instant>,
}

impl TaskEngine {
    pub async fn new(
        storage: Arc<Database>,
        http: Arc<HttpClient>,
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

    /// 重新从 DB 加载任务列表
    #[allow(dead_code)]
    pub async fn reload_tasks(&self) {
        let tasks = task_provider::load_tasks(&self.storage).await;
        *self.tasks.write().await = tasks;
    }

    // ─── 任务操作 ─────────────────────────────────────────────

    /// 启动任务
    pub async fn start_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let devices = self.storage.load_all_devices().await;
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

        // 创建新轮次
        let round_id = self.storage.create_round(task_id).await.unwrap_or(0);

        self.storage
            .save_task_state(task_id, task_status::EXECUTING, Some(&serial), Some(round_id))
            .await;
        let started_at = self.storage.start_task_run(task_id, &serial, round_id).await;

        self.spawn_loop(task_id, started_at, round_id).await;
        self.emit_update().await;
        Ok(())
    }

    /// 暂停任务
    pub async fn pause_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
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

        let run_info = {
            let mut running = self.running.write().await;
            if let Some(entry) = running.get(task_id) {
                entry.cancel.cancel();
            }
            running.remove(task_id)
        };

        if let Some(run) = run_info {
            self.storage.finish_task_run(task_id, run.started_at, run_status::PAUSED).await;
        }

        {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                rollback_running_keywords(task);
                task.status = task_status::PAUSED.to_string();
                task.assigned_device = None;
            }
        }

        self.storage.save_task_state(task_id, task_status::PAUSED, None, None).await;
        self.emit_update().await;
        Ok(())
    }

    /// 继续任务
    pub async fn resume_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let devices = self.storage.load_all_devices().await;
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
            rollback_running_keywords(task);
            task.status = task_status::EXECUTING.to_string();
            task.assigned_device = Some(serial.clone());
            serial
        };

        // resume 不创建新轮次，复用当前轮次
        let round_id = self
            .storage
            .load_task_state(task_id)
            .await
            .and_then(|(_, _, rid)| rid)
            .unwrap_or(0);

        self.storage
            .save_task_state(task_id, task_status::EXECUTING, Some(&serial), Some(round_id))
            .await;
        let started_at = self.storage.start_task_run(task_id, &serial, round_id).await;

        self.spawn_loop(task_id, started_at, round_id).await;
        self.emit_update().await;
        Ok(())
    }

    /// 取消正在运行的任务循环，结束 run 记录，清进度，重建任务
    async fn cancel_and_cleanup(&self, task_id: &str) -> Result<Option<Task>, String> {
        {
            let tasks = self.tasks.read().await;
            if !tasks.iter().any(|t| t.id == task_id) {
                return Err("任务不存在".into());
            }
        }

        let run_info = {
            let mut running = self.running.write().await;
            if let Some(entry) = running.get(task_id) {
                entry.cancel.cancel();
            }
            running.remove(task_id)
        };

        if let Some(run) = run_info {
            self.storage.finish_task_run(task_id, run.started_at, run_status::STOPPED).await;
        }
        self.storage.clear_task_progress(task_id).await;
        self.storage.delete_task_state(task_id).await;
        let fresh = task_provider::load_task_by_id(&self.storage, task_id).await;

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

        let devices = self.storage.load_all_devices().await;
        let serial = {
            let mut tasks = self.tasks.write().await;
            let serial = pick_ready_serial(&devices, &tasks)?;

            if let Some(mut fresh) = fresh {
                fresh.status = task_status::EXECUTING.to_string();
                fresh.assigned_device = Some(serial.clone());
                if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
                    tasks[pos] = fresh;
                }
            } else {
                // DB 中无此任务定义，无法重试
                return Err("任务定义不存在，无法重试".into());
            }
            serial
        };

        // retry 创建新轮次
        let round_id = self.storage.create_round(task_id).await.unwrap_or(0);

        self.storage
            .save_task_state(task_id, task_status::EXECUTING, Some(&serial), Some(round_id))
            .await;
        let started_at = self.storage.start_task_run(task_id, &serial, round_id).await;

        self.spawn_loop(task_id, started_at, round_id).await;
        self.emit_update().await;
        Ok(())
    }

    /// 释放离线设备上的任务
    pub async fn release_offline_devices(&self, online_serials: &[String]) -> u32 {
        let online_set: HashSet<&str> = online_serials.iter().map(|s| s.as_str()).collect();
        let mut released = 0u32;

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

        if !run_infos.is_empty() {
            for (tid, ri) in &run_infos {
                if let Some(run) = ri {
                    self.storage.finish_task_run(tid, run.started_at, run_status::STOPPED).await;
                }
                self.storage.save_task_state(tid, task_status::ERROR, None, None).await;
            }
            released = run_infos.len() as u32;
        }

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
        let devices = self.storage.load_all_devices().await;
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

    async fn spawn_loop(self: &Arc<Self>, task_id: &str, started_at: i64, round_id: i64) {
        let cancel = CancellationToken::new();
        self.running.write().await.insert(
            task_id.to_string(),
            RunningTask { cancel: cancel.clone(), started_at, round_id },
        );

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

            let run_info = engine.running.write().await.remove(&tid);
            if completed {
                if let Some(ref run) = run_info {
                    // 双保险：tick 中可能已调用 finish_round，这里再确保一次
                    engine.storage.finish_round(run.round_id, "completed").await;
                }
                let sa = run_info.map(|r| r.started_at).unwrap_or(started_at);
                engine.storage.finish_task_run(&tid, sa, run_status::COMPLETED).await;
            }
        });
    }

    /// 核心 tick 逻辑
    async fn tick(&self, task_id: &str) -> bool {
        // Step 1: DB 查询在写锁外执行
        let device_serial_for_check = {
            let tasks = self.tasks.read().await;
            tasks.iter().find(|t| t.id == task_id).and_then(|t| t.assigned_device.clone())
        };
        let device_online = match device_serial_for_check.as_deref() {
            Some(serial) => self
                .storage
                .get_device_by_serial(serial)
                .await
                .map(|d| d.state == crate::constants::device_state::DEVICE)
                .unwrap_or(false),
            None => false,
        };

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

            if !device_online {
                rollback_running_keywords(task);
                task.status = task_status::ERROR.to_string();
                task.assigned_device = None;
                break 'effect TickEffect::DeviceOffline {
                    task_id: task_id.to_string(),
                    device_serial,
                };
            }

            // ── 风控模拟（可配置开关） ────────────────────────
            if crate::constants::debug::MOCK_RISK_ENABLED {
                let mut rng = rand::rng();
                if rng.random_bool(crate::constants::debug::MOCK_RISK_PROBABILITY) {
                    eprintln!(
                        "[engine] risk-control triggered (simulated): task={}, device={}",
                        task_id, device_serial
                    );
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
                task.status = task_status::SUCCESS.to_string();
                task.assigned_device = None;
                break 'effect TickEffect::TaskSuccess { task_id: task_id.to_string() };
            };

            let city = &mut task.cities[active_idx];

            // 上一 tick 的 RUN → OK 时才 done += 1
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

        // Step 3: 锁外执行 DB 操作（直接 await，无需 spawn_blocking）
        match effect {
            TickEffect::KeywordDone { task_id, city_name, kw_name, device_serial } => {
                let round_id =
                    self.running.read().await.get(&task_id).map(|r| r.round_id).unwrap_or(0);
                self.storage
                    .record_keyword_done(&task_id, &city_name, &kw_name, &device_serial, round_id)
                    .await;
                false
            },
            TickEffect::TaskSuccess { task_id } => {
                // 结束当前轮次
                if let Some(run) = self.running.read().await.get(&task_id) {
                    self.storage.finish_round(run.round_id, "completed").await;
                }
                self.storage.save_task_state(&task_id, task_status::SUCCESS, None, None).await;
                true
            },
            TickEffect::DeviceOffline { task_id, device_serial } => {
                // 二次确认设备在线状态（用原device_serial，而非已置空的assigned_device）
                let still_offline = self
                    .storage
                    .get_device_by_serial(&device_serial)
                    .await
                    .map(|d| d.state != crate::constants::device_state::DEVICE)
                    .unwrap_or(true);
                if !still_offline {
                    // 设备已恢复在线，回滚内存状态
                    let mut tasks = self.tasks.write().await;
                    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                        task.status = task_status::EXECUTING.to_string();
                        task.assigned_device = Some(device_serial.clone());
                    }
                    eprintln!("[engine] 设备已恢复在线，回滚任务状态: {}", task_id);
                    return false;
                }
                self.storage.save_task_state(&task_id, task_status::ERROR, None, None).await;
                true
            },
            TickEffect::RiskControl { task_id, device_serial } => {
                self.storage.save_task_state(&task_id, task_status::ERROR, None, None).await;
                self.storage.flag_device(&device_serial).await;
                let _ = self.app_handle.emit(
                    crate::constants::tauri_event::RISK_CONTROL,
                    serde_json::json!({
                        "task_id": task_id,
                        "device_serial": device_serial,
                        "message": "设备风控触发，任务已停止，设备已标记"
                    }),
                );
                let _ = self.app_handle.emit(crate::constants::tauri_event::DEVICES_CHANGED, ());
                true
            },
            TickEffect::None => false,
        }
    }

    /// 推送任务状态到前端（节流控制）
    async fn emit_update(&self) {
        let throttle_ms = crate::constants::debug::EMIT_THROTTLE_MS;
        {
            let mut last = self.last_emit.lock().await;
            if last.elapsed() < Duration::from_millis(throttle_ms) {
                return;
            }
            *last = std::time::Instant::now();
        }
        let tasks = self.tasks.read().await;
        let snapshot = TaskSnapshot { tasks: tasks.clone() };
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

    // ─── MQTT 消息处理 ─────────────────────────────────────────

    /// 处理 MQTT 踢设备指令
    pub async fn handle_device_kick(self: &Arc<Self>, hw_serials: Vec<String>) -> u32 {
        let mut kicked = 0u32;

        for hw_serial in &hw_serials {
            let device = self.storage.get_device_by_hw_serial(hw_serial).await;
            let Some(device) = device else { continue };
            let serial = device.serial.clone();

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

            if let Some(task_id) = task_to_pause {
                if let Err(e) = self.pause_task(&task_id).await {
                    eprintln!("[engine] 踢设备时暂停任务失败: task={}, err={}", task_id, e);
                }
            }

            self.storage.delete_device(&serial).await;
            eprintln!("[engine] 设备已踢下线: hw_serial={}, serial={}", hw_serial, serial);
            kicked += 1;
        }

        if kicked > 0 {
            let _ = self.app_handle.emit(crate::constants::tauri_event::DEVICES_CHANGED, ());
            self.emit_update().await;
        }
        kicked
    }

    /// 增量合并单个任务（reload_task 核心逻辑）
    async fn merge_single_task(&self, task_id: &str) {
        let new_def = match self.http.fetch_task(task_id).await {
            Ok(def) => def,
            Err(e) => {
                eprintln!("[engine] merge_single_task: 获取任务定义失败 {}: {}", task_id, e);
                return;
            },
        };

        let payload = match serde_json::to_string(&new_def.cities) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[engine] merge_single_task: 序列化 payload 失败: {}", e);
                return;
            },
        };

        self.storage.upsert_task_def(task_id, &new_def.name, &payload, 1, "").await;

        let was_success = {
            let tasks = self.tasks.read().await;
            tasks.iter().any(|t| t.id == task_id && t.status == task_status::SUCCESS)
        };
        if was_success {
            self.storage.clear_task_progress(task_id).await;
            self.storage.delete_task_state(task_id).await;
            eprintln!("[engine] 任务 {} 原状态 SUCCESS，已清空旧进度", task_id);
        }

        let merged = task_provider::build_task(&self.storage, new_def).await;

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
        drop(tasks);

        self.storage.cleanup_orphan_progress(task_id, valid_pairs).await;
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
                    self.reloading.lock().await.remove(tid);
                }
            },
            "delete_task" => {
                if let Some(tid) = task_id {
                    eprintln!("[engine] 收到 delete_task: {}", tid);
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
                    {
                        let mut tasks = self.tasks.write().await;
                        tasks.retain(|t| t.id != tid);
                    }
                    self.storage.batch_cleanup_tasks(&[tid.to_string()]).await;
                    self.emit_update().await;
                }
            },
            _ => {
                eprintln!("[engine] 未知的 task reload action: {}", action);
            },
        }
    }

    /// 处理手机号解绑/被抢占 — 暂停+删除对应任务
    pub async fn handle_phones_unbind(self: &Arc<Self>, phones: Vec<String>) -> u32 {
        let mut all_task_ids: Vec<String> = Vec::new();
        for phone in &phones {
            let task_ids = self.storage.get_tasks_by_phone(phone).await;
            eprintln!("[engine] 解绑手机号 {}: 关联 {} 个任务", phone, task_ids.len());
            all_task_ids.extend(task_ids);
        }
        all_task_ids.sort();
        all_task_ids.dedup();

        if all_task_ids.is_empty() {
            return 0;
        }

        for task_id in &all_task_ids {
            let is_running = {
                let tasks = self.tasks.read().await;
                tasks.iter().any(|t| {
                    t.id == *task_id
                        && (t.status == crate::constants::task_status::EXECUTING
                            || t.status == crate::constants::task_status::PAUSED)
                })
            };
            if is_running {
                if let Err(e) = self.stop_task(task_id).await {
                    eprintln!("[engine] 解绑时停止任务失败: task={}, err={}", task_id, e);
                }
            }
        }

        {
            let all_ids_set: HashSet<&str> = all_task_ids.iter().map(|s| s.as_str()).collect();
            let mut tasks = self.tasks.write().await;
            tasks.retain(|t| !all_ids_set.contains(t.id.as_str()));
        }

        let removed = all_task_ids.len() as u32;

        // DB 批量清理（单连接事务内完成）
        self.storage.batch_cleanup_tasks(&all_task_ids).await;

        // 更新 synced_phones
        let current = self.storage.get_setting("synced_phones").await.unwrap_or_default();
        let mut phone_list: Vec<String> = serde_json::from_str(&current).unwrap_or_default();
        phone_list.retain(|p| !phones.contains(p));
        self.storage
            .set_setting("synced_phones", &serde_json::to_string(&phone_list).unwrap_or_default())
            .await;
        eprintln!(
            "[engine] 批量清理完成: {} 个任务, synced_phones={:?}",
            all_task_ids.len(),
            phone_list
        );

        self.emit_update().await;
        removed
    }
}
