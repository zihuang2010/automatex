//! 引擎事件循环 —— 独占 Vec<Task> 和 HashMap<RunningTask>，零锁竞争

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::Emitter;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::constants::{self, city_status, keyword_status, round_status, run_status, task_status};
use crate::http;
use crate::storage::{Database, DeviceRow};
use crate::task_provider::{self, Task};

use super::worker::spawn_worker;
use super::{EngineMsg, TaskSnapshotRef, TickOutcome};

// ─── 内部状态 ──────────────────────────────────────────

struct WorkerInfo {
    cancel: CancellationToken,
    #[allow(dead_code)]
    handle: JoinHandle<()>,
    started_at: i64,
    round_id: i64,
}

struct EngineState {
    tasks: Vec<Task>,
    running: HashMap<String, WorkerInfo>,
    reloading: HashSet<String>,
    storage: Arc<Database>,
    http: Arc<dyn http::ApiClient>,
    app_handle: tauri::AppHandle,
    tx: mpsc::Sender<EngineMsg>,
    last_emit: Instant,
    /// P1: 上次 emit 的 tasks 摘要 hash（状态去重，避免无变化时重复序列化）
    last_hash: u64,
}

// ─── 辅助函数 ──────────────────────────────────────────

fn compute_assigned_set(tasks: &[Task]) -> HashSet<&str> {
    tasks
        .iter()
        .filter(|t| t.status == task_status::EXECUTING)
        .filter_map(|t| t.assigned_device.as_deref())
        .collect()
}

fn pick_ready_serial(devices: &[DeviceRow], tasks: &[Task]) -> Result<String, String> {
    let assigned = compute_assigned_set(tasks);
    devices
        .iter()
        .find(|d| {
            d.state == constants::device_state::DEVICE
                && !d.is_flagged
                && !assigned.contains(d.serial.as_str())
        })
        .map(|d| d.serial.clone())
        .ok_or_else(|| "当前没有就绪安全的设备，请检查设备状态".to_string())
}

fn rollback_running_keywords(task: &mut Task) {
    for city in &mut task.cities {
        for kw in &mut city.keywords {
            if kw.status == keyword_status::RUN {
                kw.status = keyword_status::PENDING.to_string();
            }
        }
    }
}

// ─── 事件循环 ──────────────────────────────────────────

pub(super) fn spawn(
    rx: mpsc::Receiver<EngineMsg>,
    tx: mpsc::Sender<EngineMsg>,
    tasks: Vec<Task>,
    storage: Arc<Database>,
    http: Arc<dyn http::ApiClient>,
    app_handle: tauri::AppHandle,
) {
    let state = EngineState {
        tasks,
        running: HashMap::new(),
        reloading: HashSet::new(),
        storage,
        http,
        app_handle,
        tx,
        last_emit: Instant::now() - Duration::from_secs(1),
        last_hash: 0,
    };
    tokio::spawn(engine_loop(state, rx));
}

async fn engine_loop(mut s: EngineState, mut rx: mpsc::Receiver<EngineMsg>) {
    while let Some(msg) = rx.recv().await {
        match msg {
            // ── 生命周期 ──
            EngineMsg::StartTask { task_id, reply } => {
                let result = handle_start(&mut s, &task_id).await;
                let _ = reply.send(result);
            },
            EngineMsg::PauseTask { task_id, reply } => {
                let result = handle_pause(&mut s, &task_id).await;
                let _ = reply.send(result);
            },
            EngineMsg::ResumeTask { task_id, reply } => {
                let result = handle_resume(&mut s, &task_id).await;
                let _ = reply.send(result);
            },
            EngineMsg::StopTask { task_id, reply } => {
                let result = handle_stop(&mut s, &task_id).await;
                let _ = reply.send(result);
            },
            EngineMsg::RetryTask { task_id, reply } => {
                let result = handle_retry(&mut s, &task_id).await;
                let _ = reply.send(result);
            },

            // ── 查询 ──
            EngineMsg::GetTasks { reply } => {
                let _ = reply.send(s.tasks.clone());
            },
            EngineMsg::GetReadySerials { reply } => {
                let devices = s.storage.load_all_devices().await;
                let assigned = compute_assigned_set(&s.tasks);
                let ready: Vec<String> = devices
                    .into_iter()
                    .filter(|d| {
                        d.state == constants::device_state::DEVICE
                            && !d.is_flagged
                            && !assigned.contains(d.serial.as_str())
                    })
                    .map(|d| d.serial)
                    .collect();
                let _ = reply.send(ready);
            },

            // ── 状态管理 ──
            EngineMsg::ReorderCities { task_id, new_order, reply } => {
                let result = handle_reorder(&mut s, &task_id, new_order).await;
                let _ = reply.send(result);
            },
            EngineMsg::ReloadTasks => {
                handle_reload_tasks(&mut s).await;
            },

            // ── MQTT 处理 ──
            EngineMsg::HandleTaskReload { action, task_id } => {
                handle_task_reload_msg(&mut s, &action, task_id.as_deref()).await;
            },
            EngineMsg::HandleDeviceKick { hw_serials, reply } => {
                let n = handle_device_kick(&mut s, hw_serials).await;
                let _ = reply.send(n);
            },
            EngineMsg::HandlePhonesUnbind { phones, reply } => {
                let n = handle_phones_unbind(&mut s, phones).await;
                let _ = reply.send(n);
            },
            EngineMsg::ReleaseOfflineDevices { online_serials, reply } => {
                let n = handle_release_offline(&mut s, &online_serials).await;
                let _ = reply.send(n);
            },

            // ── Worker 回报 ──
            EngineMsg::TickRequest { task_id, device_online, reply } => {
                let outcome = process_tick(&mut s, &task_id, device_online).await;
                let _ = reply.send(outcome);
            },
            EngineMsg::WorkerExited { task_id, success } => {
                if let Some(info) = s.running.remove(&task_id) {
                    if success {
                        s.storage.finish_round(info.round_id, round_status::COMPLETED).await;
                        s.storage
                            .finish_task_run(&task_id, info.started_at, run_status::COMPLETED)
                            .await;
                    }
                }
                emit_update(&mut s).await;
            },
        }

        // 每条消息处理后自动 emit（带节流）
        emit_update(&mut s).await;
    }
}

// ─── emit 节流 ─────────────────────────────────────────

async fn emit_update(s: &mut EngineState) {
    let throttle_ms = constants::debug::EMIT_THROTTLE_MS;
    if s.last_emit.elapsed() < Duration::from_millis(throttle_ms) {
        return;
    }

    // P1: hash 去重 — 状态无变化时跳过序列化和 IPC
    let hash = compute_tasks_hash(&s.tasks);
    if hash == s.last_hash {
        return;
    }
    s.last_hash = hash;
    s.last_emit = Instant::now();
    let snapshot = TaskSnapshotRef { tasks: &s.tasks };
    let _ = s.app_handle.emit(constants::tauri_event::TASK_UPDATE, &snapshot);
}

/// 计算任务列表的轻量摘要 hash（仅基于 status/progress/device，微秒级）
fn compute_tasks_hash(tasks: &[Task]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tasks.len().hash(&mut hasher);
    for t in tasks {
        t.id.hash(&mut hasher);
        t.status.hash(&mut hasher);
        t.assigned_device.hash(&mut hasher);
        for c in &t.cities {
            c.status.hash(&mut hasher);
            c.progress.hash(&mut hasher);
            c.done.hash(&mut hasher);
        }
    }
    hasher.finish()
}

async fn force_emit(s: &mut EngineState) {
    s.last_emit = Instant::now();
    let snapshot = TaskSnapshotRef { tasks: &s.tasks };
    let _ = s.app_handle.emit(constants::tauri_event::TASK_UPDATE, &snapshot);
}

// ─── 生命周期处理 ──────────────────────────────────────

async fn handle_start(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let status = s
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .map(|t| t.status.clone())
        .ok_or("任务不存在")?;
    if status != task_status::WAITING {
        return Err("任务状态非 WAITING，无法启动".into());
    }

    let devices = s.storage.load_all_devices().await;
    let serial = pick_ready_serial(&devices, &s.tasks)?;

    let task = s.tasks.iter_mut().find(|t| t.id == task_id).unwrap();
    task.status = task_status::EXECUTING.to_string();
    task.assigned_device = Some(serial.clone());

    let round_id = match s.storage.create_round(task_id).await {
        Some(id) => id,
        None => {
            let task = s.tasks.iter_mut().find(|t| t.id == task_id).unwrap();
            task.status = task_status::WAITING.to_string();
            task.assigned_device = None;
            return Err("创建轮次失败（数据库错误），无法启动任务".into());
        },
    };

    s.storage
        .save_task_state(task_id, task_status::EXECUTING, Some(&serial), Some(round_id))
        .await;
    let started_at = s.storage.start_task_run(task_id, &serial, round_id).await;

    spawn_task_worker(s, task_id, &serial, started_at, round_id);
    Ok(())
}

async fn handle_pause(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let status = s
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .map(|t| t.status.clone())
        .ok_or("任务不存在")?;
    if status != task_status::EXECUTING {
        return Err(format!("任务状态为 {}，只有 EXECUTING 可以暂停", status));
    }

    let info = cancel_worker(s, task_id);
    let round_id = info.as_ref().map(|r| r.round_id);

    if let Some(ref run) = info {
        s.storage.finish_task_run(task_id, run.started_at, run_status::PAUSED).await;
    }

    if let Some(task) = s.tasks.iter_mut().find(|t| t.id == task_id) {
        rollback_running_keywords(task);
        task.status = task_status::PAUSED.to_string();
        task.assigned_device = None;
    }

    s.storage.save_task_state(task_id, task_status::PAUSED, None, round_id).await;
    Ok(())
}

async fn handle_resume(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let status = s
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .map(|t| t.status.clone())
        .ok_or("任务不存在")?;
    if status != task_status::PAUSED && status != task_status::ERROR {
        return Err(format!("任务状态为 {}，只有 PAUSED/ERROR 可以继续", status));
    }

    let devices = s.storage.load_all_devices().await;
    let serial = pick_ready_serial(&devices, &s.tasks)?;

    let task = s.tasks.iter_mut().find(|t| t.id == task_id).unwrap();
    rollback_running_keywords(task);
    task.status = task_status::EXECUTING.to_string();
    task.assigned_device = Some(serial.clone());

    let saved_round_id = s.storage.load_task_state(task_id).await.and_then(|(_, _, rid)| rid);

    let round_id = match saved_round_id {
        Some(rid) => rid,
        None => match s.storage.create_round(task_id).await {
            Some(id) => id,
            None => {
                let task = s.tasks.iter_mut().find(|t| t.id == task_id).unwrap();
                task.status = task_status::PAUSED.to_string();
                task.assigned_device = None;
                return Err("创建轮次失败，无法继续任务".into());
            },
        },
    };

    s.storage
        .save_task_state(task_id, task_status::EXECUTING, Some(&serial), Some(round_id))
        .await;
    let started_at = s.storage.start_task_run(task_id, &serial, round_id).await;

    spawn_task_worker(s, task_id, &serial, started_at, round_id);
    Ok(())
}

async fn handle_stop(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let fresh = cancel_and_cleanup(s, task_id).await?;
    if let Some(fresh) = fresh {
        if let Some(pos) = s.tasks.iter().position(|t| t.id == task_id) {
            s.tasks[pos] = fresh;
        }
    }
    Ok(())
}

async fn handle_retry(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let fresh = cancel_and_cleanup(s, task_id).await?;

    let devices = s.storage.load_all_devices().await;
    let serial = pick_ready_serial(&devices, &s.tasks)?;

    if let Some(mut fresh) = fresh {
        fresh.status = task_status::EXECUTING.to_string();
        fresh.assigned_device = Some(serial.clone());
        if let Some(pos) = s.tasks.iter().position(|t| t.id == task_id) {
            s.tasks[pos] = fresh;
        }
    } else {
        return Err("任务定义不存在，无法重试".into());
    }

    let round_id = match s.storage.create_round(task_id).await {
        Some(id) => id,
        None => {
            if let Some(task) = s.tasks.iter_mut().find(|t| t.id == task_id) {
                task.status = task_status::WAITING.to_string();
                task.assigned_device = None;
            }
            return Err("创建轮次失败（数据库错误），无法重试任务".into());
        },
    };

    s.storage
        .save_task_state(task_id, task_status::EXECUTING, Some(&serial), Some(round_id))
        .await;
    let started_at = s.storage.start_task_run(task_id, &serial, round_id).await;

    spawn_task_worker(s, task_id, &serial, started_at, round_id);
    Ok(())
}

// ─── 内部辅助 ──────────────────────────────────────────

fn cancel_worker(s: &mut EngineState, task_id: &str) -> Option<WorkerInfo> {
    if let Some(info) = s.running.get(task_id) {
        info.cancel.cancel();
    }
    s.running.remove(task_id)
}

async fn cancel_and_cleanup(s: &mut EngineState, task_id: &str) -> Result<Option<Task>, String> {
    if !s.tasks.iter().any(|t| t.id == task_id) {
        return Err("任务不存在".into());
    }

    let info = cancel_worker(s, task_id);

    if let Some(ref run) = info {
        s.storage.finish_round(run.round_id, round_status::STOPPED).await;
        s.storage.finish_task_run(task_id, run.started_at, run_status::STOPPED).await;
    }

    s.storage.clear_task_progress(task_id).await;
    s.storage.delete_task_state(task_id).await;
    let fresh = task_provider::load_task_by_id(&s.storage, task_id).await;
    Ok(fresh)
}

fn spawn_task_worker(
    s: &mut EngineState,
    task_id: &str,
    serial: &str,
    started_at: i64,
    round_id: i64,
) {
    let cancel = CancellationToken::new();
    let handle = spawn_worker(
        task_id.to_string(),
        serial.to_string(),
        cancel.clone(),
        s.tx.clone(),
        Arc::clone(&s.storage),
    );
    s.running
        .insert(task_id.to_string(), WorkerInfo { cancel, handle, started_at, round_id });
}

// ─── 状态管理 ──────────────────────────────────────────

async fn handle_reorder(
    s: &mut EngineState,
    task_id: &str,
    new_order: Vec<String>,
) -> Result<(), String> {
    let task = s.tasks.iter_mut().find(|t| t.id == task_id).ok_or("任务不存在")?;

    let (fixed, mut pending): (Vec<_>, Vec<_>) =
        task.cities.drain(..).partition(|c| c.status != city_status::PENDING);

    pending
        .sort_by_key(|c| new_order.iter().position(|name| name == &c.name).unwrap_or(usize::MAX));

    task.cities = fixed.into_iter().chain(pending).collect();
    s.storage.save_city_order(task_id, &new_order).await;
    Ok(())
}

async fn handle_reload_tasks(s: &mut EngineState) {
    let running_snapshot: HashMap<String, (String, Option<String>)> = s
        .tasks
        .iter()
        .filter(|t| s.running.contains_key(&t.id))
        .map(|t| (t.id.clone(), (t.status.clone(), t.assigned_device.clone())))
        .collect();

    let mut tasks = task_provider::load_tasks(&s.storage).await;
    for task in &mut tasks {
        if let Some((status, device)) = running_snapshot.get(&task.id) {
            task.status = status.clone();
            task.assigned_device = device.clone();
        }
    }
    s.tasks = tasks;
}

// ─── Tick 处理 ─────────────────────────────────────────

async fn process_tick(s: &mut EngineState, task_id: &str, device_online: bool) -> TickOutcome {
    let Some(run_info) = s.running.get(task_id) else {
        return TickOutcome::Continue;
    };
    let run_started_at = run_info.started_at;
    let run_round_id = run_info.round_id;

    let task = match s.tasks.iter_mut().find(|t| t.id == task_id) {
        Some(t) => t,
        None => return TickOutcome::Continue,
    };

    if task.status != task_status::EXECUTING {
        return TickOutcome::Continue;
    }

    let device_serial = task.assigned_device.clone().unwrap_or_default();

    // 设备离线
    if !device_online {
        // 再次确认
        let still_offline = s
            .storage
            .get_device_by_serial(&device_serial)
            .await
            .map(|d| d.state != constants::device_state::DEVICE)
            .unwrap_or(true);
        if !still_offline {
            return TickOutcome::Continue;
        }

        rollback_running_keywords(task);
        task.status = task_status::ERROR.to_string();
        task.assigned_device = None;
        s.storage.finish_task_run(task_id, run_started_at, run_status::STOPPED).await;
        s.storage
            .save_task_state(task_id, task_status::ERROR, None, Some(run_round_id))
            .await;
        return TickOutcome::TaskError;
    }

    // 风控模拟
    if constants::debug::MOCK_RISK_ENABLED {
        let triggered = {
            let mut rng = rand::rng();
            rand::RngExt::random_bool(&mut rng, constants::debug::MOCK_RISK_PROBABILITY)
        };
        if triggered {
            eprintln!(
                "[engine] risk-control triggered (simulated): task={}, device={}",
                task_id, device_serial
            );
            rollback_running_keywords(task);
            task.status = task_status::ERROR.to_string();
            task.assigned_device = None;

            s.storage.finish_task_run(task_id, run_started_at, run_status::STOPPED).await;
            s.storage
                .save_task_state(task_id, task_status::ERROR, None, Some(run_round_id))
                .await;
            s.storage.flag_device(&device_serial).await;
            let _ = s.app_handle.emit(
                constants::tauri_event::RISK_CONTROL,
                serde_json::json!({
                    "task_id": task_id,
                    "device_serial": device_serial,
                    "message": "设备风控触发，任务已停止，设备已标记"
                }),
            );
            let _ = s.app_handle.emit(constants::tauri_event::DEVICES_CHANGED, ());
            return TickOutcome::TaskError;
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
        s.storage.save_task_state(task_id, task_status::SUCCESS, None, None).await;
        return TickOutcome::TaskDone;
    };

    let city = &mut task.cities[active_idx];

    // 记录完成的关键词 (RUN → OK)
    let mut completed_kw: Option<(String, String)> = None;
    for kw in city.keywords.iter_mut() {
        if kw.status == keyword_status::RUN {
            completed_kw = Some((city.name.clone(), kw.name.clone()));
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

        if let Some((done_city, done_kw)) = completed_kw {
            s.storage
                .record_keyword_done(task_id, &done_city, &done_kw, &device_serial, run_round_id)
                .await;
        }
        TickOutcome::Continue
    } else if let Some((done_city, done_kw)) = completed_kw {
        city.status = city_status::DONE.to_string();
        city.progress = 100;

        s.storage
            .record_keyword_done(task_id, &done_city, &done_kw, &device_serial, run_round_id)
            .await;

        let has_next = task
            .cities
            .iter_mut()
            .find(|c| c.status == city_status::PENDING)
            .map(|nc| {
                nc.status = city_status::ACTIVE.to_string();
            })
            .is_some();

        if has_next {
            TickOutcome::Continue
        } else {
            task.status = task_status::SUCCESS.to_string();
            task.assigned_device = None;
            s.storage.save_task_state(task_id, task_status::SUCCESS, None, None).await;
            TickOutcome::TaskDone
        }
    } else {
        city.status = city_status::DONE.to_string();
        city.progress = 100;
        let next = task.cities.iter_mut().find(|c| c.status == city_status::PENDING);
        if let Some(nc) = next {
            nc.status = city_status::ACTIVE.to_string();
            TickOutcome::Continue
        } else {
            task.status = task_status::SUCCESS.to_string();
            task.assigned_device = None;
            s.storage.save_task_state(task_id, task_status::SUCCESS, None, None).await;
            TickOutcome::TaskDone
        }
    }
}

// ─── MQTT 处理 ─────────────────────────────────────────

async fn handle_device_kick(s: &mut EngineState, hw_serials: Vec<String>) -> u32 {
    let mut kicked = 0u32;

    for hw_serial in &hw_serials {
        let device = s.storage.get_device_by_hw_serial(hw_serial).await;
        let Some(device) = device else { continue };
        let serial = device.serial.clone();

        let task_to_pause: Option<String> = s
            .tasks
            .iter()
            .find(|t| {
                t.assigned_device.as_deref() == Some(&serial)
                    && (t.status == task_status::EXECUTING || t.status == task_status::PAUSED)
            })
            .map(|t| t.id.clone());

        if let Some(task_id) = task_to_pause {
            if let Err(e) = handle_pause(s, &task_id).await {
                eprintln!("[engine] 踢设备时暂停任务失败: task={}, err={}", task_id, e);
            }
        }

        s.storage.delete_device(&serial).await;
        eprintln!("[engine] 设备已踢下线: hw_serial={}, serial={}", hw_serial, serial);
        kicked += 1;
    }

    if kicked > 0 {
        let _ = s.app_handle.emit(constants::tauri_event::DEVICES_CHANGED, ());
    }
    kicked
}

async fn handle_phones_unbind(s: &mut EngineState, phones: Vec<String>) -> u32 {
    let mut all_task_ids: Vec<String> = Vec::new();
    for phone in &phones {
        let task_ids = s.storage.get_tasks_by_phone(phone).await;
        all_task_ids.extend(task_ids);
    }
    all_task_ids.sort();
    all_task_ids.dedup();

    if all_task_ids.is_empty() {
        return 0;
    }

    let all_ids_set: HashSet<String> = all_task_ids.iter().cloned().collect();

    // 取消 workers + 结束 runs
    for task_id in &all_task_ids {
        if let Some(info) = cancel_worker(s, task_id) {
            s.storage.finish_task_run(task_id, info.started_at, run_status::STOPPED).await;
        }
    }

    // 移除任务
    s.tasks.retain(|t| !all_ids_set.contains(&t.id));
    s.storage.batch_cleanup_tasks(&all_task_ids).await;

    let removed = all_task_ids.len() as u32;
    eprintln!("[engine] 批量清理完成: {} 个任务（解绑手机号: {:?}）", removed, phones);
    removed
}

async fn handle_task_reload_msg(s: &mut EngineState, action: &str, task_id: Option<&str>) {
    match action {
        "reload_all" => {
            eprintln!("[engine] 收到 reload_all，重新加载所有任务");
            handle_reload_tasks(s).await;
            force_emit(s).await;
        },
        "reload_task" => {
            if let Some(tid) = task_id {
                if s.reloading.contains(tid) {
                    eprintln!("[engine] reload_task 防重入跳过: {}", tid);
                    return;
                }
                s.reloading.insert(tid.to_string());

                eprintln!("[engine] 收到 reload_task: {}", tid);
                merge_single_task(s, tid).await;
                force_emit(s).await;
                s.reloading.remove(tid);
            }
        },
        "delete_task" => {
            if let Some(tid) = task_id {
                eprintln!("[engine] 收到 delete_task: {}", tid);
                let is_running = s.tasks.iter().any(|t| {
                    t.id == tid
                        && (t.status == task_status::EXECUTING
                            || t.status == task_status::PAUSED
                            || t.status == task_status::ERROR)
                });
                if is_running {
                    let _ = handle_stop(s, tid).await;
                }
                s.tasks.retain(|t| t.id != tid);
                s.storage.batch_cleanup_tasks(&[tid.to_string()]).await;
                force_emit(s).await;
            }
        },
        _ => {
            eprintln!("[engine] 未知的 task reload action: {}", action);
        },
    }
}

async fn merge_single_task(s: &mut EngineState, task_id: &str) {
    let new_def = match s.http.fetch_task(task_id).await {
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

    s.storage.upsert_task_def(task_id, &new_def.name, &payload, 1, "").await;

    let was_success = s.tasks.iter().any(|t| t.id == task_id && t.status == task_status::SUCCESS);
    if was_success {
        s.storage.clear_task_progress(task_id).await;
        s.storage.delete_task_state(task_id).await;
    }

    let merged = task_provider::build_task(&s.storage, new_def).await;

    if let Some(local) = s.tasks.iter_mut().find(|t| t.id == task_id) {
        let is_running = local.status == task_status::EXECUTING
            || local.status == task_status::PAUSED
            || local.status == task_status::ERROR;

        if is_running {
            let saved_status = local.status.clone();
            let saved_device = local.assigned_device.clone();
            let old_cities = std::mem::take(&mut local.cities);
            *local = merged;
            local.status = saved_status;
            local.assigned_device = saved_device;
            for city in &mut local.cities {
                if let Some(old_city) = old_cities.iter().find(|c| c.name == city.name) {
                    city.status = old_city.status.clone();
                    city.done = old_city.done;
                    city.progress = old_city.progress;
                    for kw in &mut city.keywords {
                        if let Some(old_kw) = old_city.keywords.iter().find(|k| k.name == kw.name) {
                            kw.status = old_kw.status.clone();
                        }
                    }
                }
            }
        } else {
            *local = merged;
        }
    } else {
        s.tasks.push(merged);
    }

    let valid_pairs: Vec<(String, String)> = s
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .map(|t| {
            t.cities
                .iter()
                .flat_map(|c| c.keywords.iter().map(move |k| (c.name.clone(), k.name.clone())))
                .collect()
        })
        .unwrap_or_default();

    s.storage.cleanup_orphan_progress(task_id, valid_pairs).await;
}

async fn handle_release_offline(s: &mut EngineState, online_serials: &[String]) -> u32 {
    let online_set: HashSet<&str> = online_serials.iter().map(|s| s.as_str()).collect();
    let mut released = 0u32;

    let task_ids_to_release: Vec<String> = s
        .tasks
        .iter()
        .filter(|t| {
            t.assigned_device.is_some()
                && t.status == task_status::EXECUTING
                && !online_set.contains(t.assigned_device.as_deref().unwrap_or(""))
        })
        .map(|t| t.id.clone())
        .collect();

    for task_id in &task_ids_to_release {
        if let Some(info) = cancel_worker(s, task_id) {
            s.storage.finish_task_run(task_id, info.started_at, run_status::STOPPED).await;
            let round_id = info.round_id;
            s.storage
                .save_task_state(task_id, task_status::ERROR, None, Some(round_id))
                .await;
        }

        if let Some(task) = s.tasks.iter_mut().find(|t| t.id == *task_id) {
            task.status = task_status::ERROR.to_string();
            task.assigned_device = None;
        }
        released += 1;
    }

    released
}
