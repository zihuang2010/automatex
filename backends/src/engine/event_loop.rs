//! 引擎事件循环 —— 独占任务明细，并维护轻量运行时与中央调度器

use std::cmp::Ordering;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant as StdInstant};

use tauri::Emitter;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant as TokioInstant;
use tokio_util::sync::CancellationToken;

use crate::constants::{self, city_status, keyword_status, round_status, run_status, task_status};
use crate::http;
use crate::storage::{Database, DeviceRow};
use crate::task_provider::{self, summarize_task, Task, TaskSummary};
use crate::task_sync;

use super::worker::spawn_worker;
use super::{EngineMsg, ExecutionOutcome, TaskSummarySnapshotRef};

struct WorkerInfo {
    cancel: CancellationToken,
    #[allow(dead_code)]
    handle: JoinHandle<()>,
    worker_seq: u64,
}

struct RunningTaskState {
    round_id: i64,
    started_at: i64,
    device_serial: String,
    active_worker: Option<WorkerInfo>,
    next_wakeup_at: Option<TokioInstant>,
    wakeup_seq: u64,
    attempt: i32,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SchedulerWakeup {
    due_at: TokioInstant,
    task_id: String,
    wakeup_seq: u64,
}

impl Ord for SchedulerWakeup {
    fn cmp(&self, other: &Self) -> Ordering {
        self.due_at
            .cmp(&other.due_at)
            .then_with(|| self.wakeup_seq.cmp(&other.wakeup_seq))
            .then_with(|| self.task_id.cmp(&other.task_id))
    }
}

impl PartialOrd for SchedulerWakeup {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct EngineState {
    tasks: Vec<Task>,
    running: HashMap<String, RunningTaskState>,
    wakeups: BinaryHeap<Reverse<SchedulerWakeup>>,
    reloading: HashSet<String>,
    storage: Arc<Database>,
    http: Arc<dyn http::ApiClient>,
    app_handle: tauri::AppHandle,
    tx: mpsc::Sender<EngineMsg>,
    snapshot_tx: watch::Sender<Vec<TaskSummary>>,
    last_emit: StdInstant,
    next_worker_seq: u64,
    next_wakeup_seq: u64,
    last_hash: u64,
}

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
        for keyword in &mut city.keywords {
            if keyword.status == keyword_status::RUN {
                keyword.status = keyword_status::PENDING.to_string();
            }
        }
    }
}

fn clear_execution_cursor(task: &mut Task) {
    task.current_city_name = None;
    task.current_keyword_name = None;
}

fn persist_cursor_candidates(
    task: &mut Task,
    city_idx: usize,
    keyword_idx: usize,
    mark_running: bool,
) {
    for city in &mut task.cities {
        if city.status == city_status::ACTIVE {
            city.status = city_status::PENDING.to_string();
        }
        for keyword in &mut city.keywords {
            if keyword.status == keyword_status::RUN {
                keyword.status = keyword_status::PENDING.to_string();
            }
        }
    }

    if let Some(city) = task.cities.get_mut(city_idx) {
        city.status = city_status::ACTIVE.to_string();
        task.current_city_name = Some(city.name.clone());
        if let Some(keyword) = city.keywords.get_mut(keyword_idx) {
            task.current_keyword_name = Some(keyword.name.clone());
            if mark_running && keyword.status == keyword_status::PENDING {
                keyword.status = keyword_status::RUN.to_string();
            }
        } else {
            task.current_keyword_name = None;
        }
    } else {
        clear_execution_cursor(task);
    }
}

fn find_keyword_index_by_name(task: &Task, city_idx: usize, keyword_name: &str) -> Option<usize> {
    task.cities
        .get(city_idx)?
        .keywords
        .iter()
        .position(|kw| kw.name == keyword_name && kw.status != keyword_status::OK)
}

fn keyword_index_by_name(task: &Task, city_idx: usize, keyword_name: &str) -> Option<usize> {
    task.cities
        .get(city_idx)?
        .keywords
        .iter()
        .position(|kw| kw.name == keyword_name)
}

fn first_pending_keyword_idx(task: &Task, city_idx: usize) -> Option<usize> {
    task.cities
        .get(city_idx)?
        .keywords
        .iter()
        .position(|k| k.status != keyword_status::OK)
}

fn ensure_execution_cursor(task: &mut Task, mark_running: bool) -> Option<(usize, usize)> {
    if let Some(ref city_name) = task.current_city_name {
        if let Some(city_idx) = task
            .cities
            .iter()
            .position(|city| city.name == *city_name && city.status != city_status::DONE)
        {
            let keyword_idx = task
                .current_keyword_name
                .as_deref()
                .and_then(|keyword_name| find_keyword_index_by_name(task, city_idx, keyword_name))
                .or_else(|| first_pending_keyword_idx(task, city_idx));

            if let Some(keyword_idx) = keyword_idx {
                persist_cursor_candidates(task, city_idx, keyword_idx, mark_running);
                return Some((city_idx, keyword_idx));
            }
        }
    }

    let next_city_idx = task
        .cities
        .iter()
        .position(|city| {
            city.status == city_status::ACTIVE
                && city.keywords.iter().any(|kw| kw.status != keyword_status::OK)
        })
        .or_else(|| {
            task.cities
                .iter()
                .position(|city| city.keywords.iter().any(|kw| kw.status != keyword_status::OK))
        })?;
    let next_keyword_idx = first_pending_keyword_idx(task, next_city_idx)?;
    persist_cursor_candidates(task, next_city_idx, next_keyword_idx, mark_running);
    Some((next_city_idx, next_keyword_idx))
}

fn move_to_next_cursor(task: &mut Task, mark_running: bool) -> Option<(usize, usize)> {
    let current_city_idx = task
        .current_city_name
        .as_deref()
        .and_then(|city_name| task.cities.iter().position(|city| city.name == city_name))
        .or_else(|| ensure_execution_cursor(task, false).map(|(city_idx, _)| city_idx))?;
    let current_keyword_idx = task
        .current_keyword_name
        .as_deref()
        .and_then(|keyword| keyword_index_by_name(task, current_city_idx, keyword))
        .or_else(|| first_pending_keyword_idx(task, current_city_idx))?;

    if let Some(next_idx) = task.cities[current_city_idx]
        .keywords
        .iter()
        .enumerate()
        .skip(current_keyword_idx + 1)
        .find(|(_, kw)| kw.status != keyword_status::OK)
        .map(|(idx, _)| idx)
    {
        persist_cursor_candidates(task, current_city_idx, next_idx, mark_running);
        return Some((current_city_idx, next_idx));
    }

    if task.cities[current_city_idx].done >= task.cities[current_city_idx].total {
        task.cities[current_city_idx].status = city_status::DONE.to_string();
        task.cities[current_city_idx].progress = 100;
    }

    for next_city_idx in current_city_idx + 1..task.cities.len() {
        if let Some(next_keyword_idx) = first_pending_keyword_idx(task, next_city_idx) {
            persist_cursor_candidates(task, next_city_idx, next_keyword_idx, mark_running);
            return Some((next_city_idx, next_keyword_idx));
        }
    }

    clear_execution_cursor(task);
    None
}

fn task_state_cursor(task: &Task) -> (Option<&str>, Option<&str>) {
    (task.current_city_name.as_deref(), task.current_keyword_name.as_deref())
}

fn build_summaries(tasks: &[Task]) -> Vec<TaskSummary> {
    tasks.iter().map(summarize_task).collect()
}

/// interval_waiting 判断：task 处于 executing 且所有关键词都是 pending
fn is_all_keywords_pending(task: &Task) -> bool {
    task.cities
        .iter()
        .all(|city| city.keywords.iter().all(|kw| kw.status == keyword_status::PENDING))
}

fn compute_summaries_hash(summaries: &[TaskSummary]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    summaries.len().hash(&mut hasher);
    for summary in summaries {
        summary.id.hash(&mut hasher);
        summary.status.hash(&mut hasher);
        summary.assigned_device.hash(&mut hasher);
        summary.keyword_done.hash(&mut hasher);
        summary.keyword_total.hash(&mut hasher);
        summary.progress.hash(&mut hasher);
        summary.current_city_name.hash(&mut hasher);
        summary.current_keyword_name.hash(&mut hasher);
        summary.round_no.hash(&mut hasher);
        summary.next_round_at.hash(&mut hasher);
    }
    hasher.finish()
}

fn clear_task_schedule(run: &mut RunningTaskState) {
    run.next_wakeup_at = None;
    run.wakeup_seq = 0;
}

fn wakeup_unix(next_wakeup_at: Option<TokioInstant>) -> Option<i64> {
    next_wakeup_at.map(|due_at| {
        let now_unix = constants::now_unix();
        let delta = due_at
            .checked_duration_since(TokioInstant::now())
            .unwrap_or_else(|| Duration::from_secs(0));
        now_unix + delta.as_secs() as i64
    })
}

async fn persist_runtime_state(s: &EngineState, task_id: &str) {
    let Some(task) = s.tasks.iter().find(|task| task.id == task_id) else {
        return;
    };
    let cursor = task_state_cursor(task);
    let runtime = s.running.get(task_id);
    let assigned_device = task.assigned_device.as_deref();
    let current_round_id = runtime.map(|run| run.round_id).or(task.current_round_id);
    let attempt = runtime.map(|run| run.attempt).unwrap_or(0);
    let next_wakeup_at = runtime.and_then(|run| wakeup_unix(run.next_wakeup_at));
    let last_error = runtime.and_then(|run| run.last_error.as_deref());
    let runtime_status = if task.status == task_status::EXECUTING {
        runtime
            .map(|run| {
                if run.active_worker.is_some() {
                    "executing"
                } else if run.next_wakeup_at.is_some() && is_all_keywords_pending(task) {
                    // 所有关键词都是 pending + 有 wakeup = 轮次间隔等待
                    "interval_waiting"
                } else if run.next_wakeup_at.is_some() {
                    "scheduled"
                } else {
                    "idle"
                }
            })
            .or(Some("scheduled"))
    } else {
        None
    };

    s.storage
        .save_task_state(
            task_id,
            &task.status,
            assigned_device,
            current_round_id,
            cursor.0,
            cursor.1,
            attempt,
            next_wakeup_at,
            last_error,
            runtime_status,
        )
        .await;
}

fn schedule_task(s: &mut EngineState, task_id: &str, delay_ms: u64) {
    let due_at = TokioInstant::now() + Duration::from_millis(delay_ms);
    if let Some(runtime) = s.running.get_mut(task_id) {
        s.next_wakeup_seq += 1;
        runtime.wakeup_seq = s.next_wakeup_seq;
        runtime.next_wakeup_at = Some(due_at);
        s.wakeups.push(Reverse(SchedulerWakeup {
            due_at,
            task_id: task_id.to_string(),
            wakeup_seq: runtime.wakeup_seq,
        }));
    }
}

fn next_due_at(s: &EngineState) -> Option<TokioInstant> {
    s.wakeups.peek().map(|item| item.0.due_at)
}

fn remove_runtime(s: &mut EngineState, task_id: &str) -> Option<RunningTaskState> {
    let mut runtime = s.running.remove(task_id)?;
    if let Some(worker) = runtime.active_worker.take() {
        worker.cancel.cancel();
    }
    clear_task_schedule(&mut runtime);
    Some(runtime)
}

fn spawn_task_worker(s: &mut EngineState, task_id: &str) {
    let Some(runtime) = s.running.get_mut(task_id) else {
        return;
    };
    if runtime.active_worker.is_some() {
        return;
    }

    let worker_seq = s.next_worker_seq;
    s.next_worker_seq += 1;
    let cancel = CancellationToken::new();
    let handle = spawn_worker(
        task_id.to_string(),
        runtime.device_serial.clone(),
        worker_seq,
        cancel.clone(),
        s.tx.clone(),
        Arc::clone(&s.storage),
    );
    runtime.active_worker = Some(WorkerInfo { cancel, handle, worker_seq });
    runtime.next_wakeup_at = None;
}

pub(super) fn spawn(
    rx: mpsc::Receiver<EngineMsg>,
    tx: mpsc::Sender<EngineMsg>,
    snapshot_tx: watch::Sender<Vec<TaskSummary>>,
    tasks: Vec<Task>,
    storage: Arc<Database>,
    http: Arc<dyn http::ApiClient>,
    app_handle: tauri::AppHandle,
) {
    let state = EngineState {
        tasks,
        running: HashMap::new(),
        wakeups: BinaryHeap::new(),
        reloading: HashSet::new(),
        storage,
        http,
        app_handle,
        tx,
        snapshot_tx,
        last_emit: StdInstant::now() - Duration::from_secs(1),
        next_worker_seq: 1,
        next_wakeup_seq: 1,
        last_hash: 0,
    };
    tokio::spawn(engine_loop(state, rx));
}

async fn engine_loop(mut s: EngineState, mut rx: mpsc::Receiver<EngineMsg>) {
    loop {
        let next_due = next_due_at(&s);

        let maybe_msg = if let Some(due_at) = next_due {
            tokio::select! {
                maybe_msg = rx.recv() => maybe_msg,
                _ = tokio::time::sleep_until(due_at) => {
                    dispatch_due_wakeups(&mut s).await;
                    emit_update(&mut s).await;
                    continue;
                }
            }
        } else {
            rx.recv().await
        };

        let Some(msg) = maybe_msg else { break };

        match msg {
            EngineMsg::StartTask { task_id, reply } => {
                let _ = reply.send(handle_start(&mut s, &task_id).await);
            },
            EngineMsg::PauseTask { task_id, reply } => {
                let _ = reply.send(handle_pause(&mut s, &task_id).await);
            },
            EngineMsg::ResumeTask { task_id, reply } => {
                let _ = reply.send(handle_resume(&mut s, &task_id).await);
            },
            EngineMsg::StopTask { task_id, reply } => {
                let _ = reply.send(handle_stop(&mut s, &task_id).await);
            },
            EngineMsg::RetryTask { task_id, reply } => {
                let _ = reply.send(handle_retry(&mut s, &task_id).await);
            },
            EngineMsg::GetTasks { reply } => {
                let _ = reply.send(build_summaries(&s.tasks));
            },
            EngineMsg::GetTaskDetail { task_id, reply } => {
                let _ = reply.send(s.tasks.iter().find(|task| task.id == task_id).cloned());
            },
            EngineMsg::GetReadySerials { reply } => {
                let devices = s.storage.load_all_devices().await;
                let assigned = compute_assigned_set(&s.tasks);
                let ready = devices
                    .into_iter()
                    .filter(|device| {
                        device.state == constants::device_state::DEVICE
                            && !device.is_flagged
                            && !assigned.contains(device.serial.as_str())
                    })
                    .map(|device| device.serial)
                    .collect();
                let _ = reply.send(ready);
            },
            EngineMsg::ReorderCities { task_id, new_order, reply } => {
                let _ = reply.send(handle_reorder(&mut s, &task_id, new_order).await);
            },
            EngineMsg::ReloadTasks => {
                handle_reload_tasks(&mut s).await;
            },
            EngineMsg::HandleTaskReload { action, task_id } => {
                handle_task_reload_msg(&mut s, &action, task_id.as_deref()).await;
            },
            EngineMsg::HandleDeviceKick { hw_serials, reply } => {
                let _ = reply.send(handle_device_kick(&mut s, hw_serials).await);
            },
            EngineMsg::HandlePhonesUnbind { phones, reply } => {
                let _ = reply.send(handle_phones_unbind(&mut s, phones).await);
            },
            EngineMsg::ReleaseOfflineDevices { online_serials, reply } => {
                let _ = reply.send(handle_release_offline(&mut s, &online_serials).await);
            },
            EngineMsg::WorkerResult { task_id, worker_seq, outcome } => {
                handle_worker_result(&mut s, &task_id, worker_seq, outcome).await;
            },
            EngineMsg::Shutdown { reply } => {
                for (task_id, runtime) in s.running.drain() {
                    if let Some(worker) = runtime.active_worker {
                        worker.cancel.cancel();
                    }
                    eprintln!("[engine] shutdown: cancelled runtime {}", task_id);
                }
                let _ = reply.send(());
                break;
            },
        }

        emit_update(&mut s).await;
    }
}

async fn dispatch_due_wakeups(s: &mut EngineState) {
    let now = TokioInstant::now();
    while let Some(Reverse(wakeup)) = s.wakeups.peek().cloned() {
        if wakeup.due_at > now {
            break;
        }
        let _ = s.wakeups.pop();

        let should_spawn = s
            .running
            .get(&wakeup.task_id)
            .map(|runtime| {
                runtime.wakeup_seq == wakeup.wakeup_seq
                    && runtime.next_wakeup_at == Some(wakeup.due_at)
                    && runtime.active_worker.is_none()
            })
            .unwrap_or(false);
        if !should_spawn {
            continue;
        }

        // interval_waiting 唤醒：恢复执行游标并创建新 run 记录
        let is_interval_wakeup = s
            .tasks
            .iter()
            .find(|t| t.id == wakeup.task_id)
            .map(|t| t.status == task_status::EXECUTING && is_all_keywords_pending(t))
            .unwrap_or(false);

        if is_interval_wakeup {
            // 恢复执行游标
            if let Some(task) = s.tasks.iter_mut().find(|t| t.id == wakeup.task_id) {
                let _ = ensure_execution_cursor(task, true);
            }
            // 创建新的 task_run 记录
            if let Some(runtime) = s.running.get(&wakeup.task_id) {
                let _ = s
                    .storage
                    .start_task_run(&wakeup.task_id, &runtime.device_serial, runtime.round_id)
                    .await;
            }
            eprintln!("[engine] interval_waiting 唤醒: task={}", wakeup.task_id);
        }

        spawn_task_worker(s, &wakeup.task_id);
        persist_runtime_state(s, &wakeup.task_id).await;
    }
}

async fn emit_update(s: &mut EngineState) {
    let throttle_ms = constants::debug::EMIT_THROTTLE_MS;
    if s.last_emit.elapsed() < Duration::from_millis(throttle_ms) {
        return;
    }

    let mut summaries = build_summaries(&s.tasks);
    inject_next_round_at(s, &mut summaries);
    let hash = compute_summaries_hash(&summaries);
    if hash == s.last_hash {
        return;
    }

    s.last_hash = hash;
    s.last_emit = StdInstant::now();
    let _ = s.snapshot_tx.send(summaries.clone());
    let snapshot = TaskSummarySnapshotRef { tasks: summaries.as_slice() };
    let _ = s.app_handle.emit(constants::tauri_event::TASK_UPDATE, &snapshot);
}

async fn force_emit(s: &mut EngineState) {
    s.last_emit = StdInstant::now();
    let mut summaries = build_summaries(&s.tasks);
    inject_next_round_at(s, &mut summaries);
    s.last_hash = compute_summaries_hash(&summaries);
    let _ = s.snapshot_tx.send(summaries.clone());
    let snapshot = TaskSummarySnapshotRef { tasks: summaries.as_slice() };
    let _ = s.app_handle.emit(constants::tauri_event::TASK_UPDATE, &snapshot);
}

/// 将 interval_waiting 的 next_wakeup_at 注入到 TaskSummary.next_round_at
fn inject_next_round_at(s: &EngineState, summaries: &mut [TaskSummary]) {
    for summary in summaries.iter_mut() {
        if let Some(runtime) = s.running.get(&summary.id) {
            summary.next_round_at = wakeup_unix(runtime.next_wakeup_at);
        }
    }
}

async fn handle_start(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let status = s
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .map(|task| task.status.clone())
        .ok_or("任务不存在")?;
    if status != task_status::WAITING {
        return Err("任务状态非 WAITING，无法启动".into());
    }

    let devices = s.storage.load_all_devices().await;
    let serial = pick_ready_serial(&devices, &s.tasks)?;

    let task = s.tasks.iter_mut().find(|task| task.id == task_id).unwrap();
    task.status = task_status::EXECUTING.to_string();
    task.assigned_device = Some(serial.clone());
    let _ = ensure_execution_cursor(task, true);

    let round_id = match s.storage.create_round(task_id).await {
        Some(round_id) => round_id,
        None => {
            let task = s.tasks.iter_mut().find(|task| task.id == task_id).unwrap();
            task.status = task_status::WAITING.to_string();
            task.assigned_device = None;
            clear_execution_cursor(task);
            return Err("创建轮次失败（数据库错误），无法启动任务".into());
        },
    };

    let started_at = s.storage.start_task_run(task_id, &serial, round_id).await;
    s.running.insert(
        task_id.to_string(),
        RunningTaskState {
            round_id,
            started_at,
            device_serial: serial,
            active_worker: None,
            next_wakeup_at: None,
            wakeup_seq: 0,
            attempt: 0,
            last_error: None,
        },
    );
    schedule_task(s, task_id, constants::timing::TASK_DISPATCH_INTERVAL_SECS * 1000);
    persist_runtime_state(s, task_id).await;
    Ok(())
}

async fn handle_pause(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let status = s
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .map(|task| task.status.clone())
        .ok_or("任务不存在")?;
    if status != task_status::EXECUTING {
        return Err(format!("任务状态为 {}，只有 EXECUTING 可以暂停", status));
    }

    let runtime = remove_runtime(s, task_id);
    let round_id = runtime.as_ref().map(|run| run.round_id);

    if let Some(run) = runtime {
        s.storage.finish_round(run.round_id, round_status::STOPPED).await;
        s.storage.finish_task_run(task_id, run.started_at, run_status::PAUSED).await;
    }

    if let Some(task) = s.tasks.iter_mut().find(|task| task.id == task_id) {
        rollback_running_keywords(task);
        task.status = task_status::PAUSED.to_string();
        task.assigned_device = None;
        let _ = ensure_execution_cursor(task, false);
    }

    let task = s.tasks.iter().find(|task| task.id == task_id).unwrap();
    let cursor = task_state_cursor(task);
    s.storage
        .save_task_state(
            task_id,
            task_status::PAUSED,
            None,
            round_id,
            cursor.0,
            cursor.1,
            0,
            None,
            None,
            Some("paused"),
        )
        .await;
    Ok(())
}

async fn handle_resume(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let status = s
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .map(|task| task.status.clone())
        .ok_or("任务不存在")?;
    if status != task_status::PAUSED && status != task_status::ERROR {
        return Err(format!("任务状态为 {}，只有 PAUSED/ERROR 可以继续", status));
    }

    let devices = s.storage.load_all_devices().await;
    let serial = pick_ready_serial(&devices, &s.tasks)?;
    let saved_state = s.storage.load_task_state(task_id).await;

    let task = s.tasks.iter_mut().find(|task| task.id == task_id).unwrap();
    rollback_running_keywords(task);
    task.status = task_status::EXECUTING.to_string();
    task.assigned_device = Some(serial.clone());
    let _ = ensure_execution_cursor(task, true);

    let round_id = match saved_state.as_ref().and_then(|state| state.current_round_id) {
        Some(round_id) => {
            if !s.storage.resume_round(round_id).await {
                return Err("恢复轮次失败，无法继续任务".into());
            }
            round_id
        },
        None => s
            .storage
            .create_round(task_id)
            .await
            .ok_or_else(|| "创建轮次失败，无法继续任务".to_string())?,
    };

    let started_at = s.storage.start_task_run(task_id, &serial, round_id).await;
    s.running.insert(
        task_id.to_string(),
        RunningTaskState {
            round_id,
            started_at,
            device_serial: serial,
            active_worker: None,
            next_wakeup_at: None,
            wakeup_seq: 0,
            attempt: saved_state.as_ref().map(|state| state.attempt).unwrap_or(0),
            last_error: saved_state.and_then(|state| state.last_error),
        },
    );
    schedule_task(s, task_id, constants::timing::TASK_DISPATCH_INTERVAL_SECS * 1000);
    persist_runtime_state(s, task_id).await;
    Ok(())
}

async fn handle_stop(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let fresh = cancel_and_cleanup(s, task_id).await?;
    if let Some(fresh) = fresh {
        if let Some(pos) = s.tasks.iter().position(|task| task.id == task_id) {
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
        let _ = ensure_execution_cursor(&mut fresh, true);
        if let Some(pos) = s.tasks.iter().position(|task| task.id == task_id) {
            s.tasks[pos] = fresh;
        }
    } else {
        return Err("任务定义不存在，无法重试".into());
    }

    let round_id = s
        .storage
        .create_round(task_id)
        .await
        .ok_or_else(|| "创建轮次失败（数据库错误），无法重试任务".to_string())?;
    let started_at = s.storage.start_task_run(task_id, &serial, round_id).await;

    s.running.insert(
        task_id.to_string(),
        RunningTaskState {
            round_id,
            started_at,
            device_serial: serial,
            active_worker: None,
            next_wakeup_at: None,
            wakeup_seq: 0,
            attempt: 0,
            last_error: None,
        },
    );
    schedule_task(s, task_id, constants::timing::TASK_DISPATCH_INTERVAL_SECS * 1000);
    persist_runtime_state(s, task_id).await;
    Ok(())
}

async fn cancel_and_cleanup(s: &mut EngineState, task_id: &str) -> Result<Option<Task>, String> {
    if !s.tasks.iter().any(|task| task.id == task_id) {
        return Err("任务不存在".into());
    }

    if let Some(runtime) = remove_runtime(s, task_id) {
        s.storage.finish_round(runtime.round_id, round_status::STOPPED).await;
        s.storage
            .finish_task_run(task_id, runtime.started_at, run_status::STOPPED)
            .await;
    }

    s.storage.clear_task_progress(task_id).await;
    s.storage.delete_task_state(task_id).await;
    Ok(task_provider::load_task_by_id(&s.storage, task_id).await)
}

async fn handle_reorder(
    s: &mut EngineState,
    task_id: &str,
    new_order: Vec<String>,
) -> Result<(), String> {
    let task = s.tasks.iter_mut().find(|task| task.id == task_id).ok_or("任务不存在")?;

    let (fixed, mut pending): (Vec<_>, Vec<_>) =
        task.cities.drain(..).partition(|city| city.status != city_status::PENDING);
    pending.sort_by_key(|city| {
        new_order.iter().position(|name| name == &city.name).unwrap_or(usize::MAX)
    });
    task.cities = fixed.into_iter().chain(pending).collect();
    s.storage.save_city_order(task_id, &new_order).await;
    Ok(())
}

async fn handle_reload_tasks(s: &mut EngineState) {
    let running_snapshot: HashMap<
        String,
        (String, Option<String>, Option<String>, Option<String>),
    > = s
        .tasks
        .iter()
        .filter(|task| s.running.contains_key(&task.id))
        .map(|task| {
            (
                task.id.clone(),
                (
                    task.status.clone(),
                    task.assigned_device.clone(),
                    task.current_city_name.clone(),
                    task.current_keyword_name.clone(),
                ),
            )
        })
        .collect();

    let mut tasks = task_provider::load_tasks(&s.storage).await;
    for task in &mut tasks {
        if let Some((status, device, city, keyword)) = running_snapshot.get(&task.id) {
            task.status = status.clone();
            task.assigned_device = device.clone();
            task.current_city_name = city.clone();
            task.current_keyword_name = keyword.clone();
            let _ = ensure_execution_cursor(task, status == task_status::EXECUTING);
        }
    }
    s.tasks = tasks;
}

async fn handle_worker_result(
    s: &mut EngineState,
    task_id: &str,
    worker_seq: u64,
    outcome: ExecutionOutcome,
) {
    let Some(runtime) = s.running.get_mut(task_id) else {
        return;
    };
    let Some(active_worker_seq) = runtime.active_worker.as_ref().map(|worker| worker.worker_seq)
    else {
        return;
    };
    if active_worker_seq != worker_seq {
        return;
    }
    runtime.active_worker = None;

    match outcome {
        ExecutionOutcome::Cancelled => {},
        ExecutionOutcome::DeviceOffline => {
            mark_task_error(
                s,
                task_id,
                "设备离线，任务已暂停".to_string(),
                false,
                run_status::STOPPED,
            )
            .await;
        },
        ExecutionOutcome::Success { next_delay_ms } => {
            if handle_success_outcome(s, task_id, next_delay_ms).await.is_err() {
                mark_task_error(
                    s,
                    task_id,
                    "任务执行游标异常".to_string(),
                    false,
                    run_status::STOPPED,
                )
                .await;
            }
        },
    }
}

async fn handle_success_outcome(
    s: &mut EngineState,
    task_id: &str,
    next_delay_ms: u64,
) -> Result<(), String> {
    let (round_id, started_at, device_serial) = {
        let runtime = s.running.get(task_id).ok_or("运行时不存在")?;
        (runtime.round_id, runtime.started_at, runtime.device_serial.clone())
    };

    let task = s.tasks.iter_mut().find(|task| task.id == task_id).ok_or("任务不存在")?;

    if task.status != task_status::EXECUTING {
        return Ok(());
    }

    if constants::debug::MOCK_RISK_ENABLED {
        let triggered = {
            let mut rng = rand::rng();
            rand::RngExt::random_bool(&mut rng, constants::debug::MOCK_RISK_PROBABILITY)
        };
        if triggered {
            mark_task_error(
                s,
                task_id,
                "设备风控触发，任务已停止，设备已标记".to_string(),
                true,
                run_status::STOPPED,
            )
            .await;
            return Ok(());
        }
    }

    let Some((city_idx, keyword_idx)) = ensure_execution_cursor(task, true) else {
        complete_task_success(s, task_id, round_id, started_at).await;
        return Ok(());
    };

    let done_city = task.cities[city_idx].name.clone();
    let done_keyword = task.cities[city_idx].keywords[keyword_idx].name.clone();
    task.cities[city_idx].keywords[keyword_idx].status = keyword_status::OK.to_string();
    task.cities[city_idx].done += 1;
    task.cities[city_idx].progress = if task.cities[city_idx].total > 0 {
        ((task.cities[city_idx].done as f64 / task.cities[city_idx].total as f64) * 100.0).round()
            as i32
    } else {
        0
    };

    s.storage
        .record_keyword_done(task_id, &done_city, &done_keyword, &device_serial, round_id)
        .await;

    if move_to_next_cursor(task, true).is_some() {
        if let Some(runtime) = s.running.get_mut(task_id) {
            runtime.attempt = 0;
            runtime.last_error = None;
        }
        schedule_task(s, task_id, next_delay_ms);
        persist_runtime_state(s, task_id).await;
    } else {
        // 所有关键词执行完毕 — 检查是否需要循环
        let interval = s
            .tasks
            .iter()
            .find(|t| t.id == task_id)
            .and_then(|t| t.interval_minute)
            .unwrap_or(0);

        if interval > 0 {
            complete_round_with_interval(s, task_id, round_id, started_at, interval).await;
        } else {
            complete_task_success(s, task_id, round_id, started_at).await;
        }
    }

    Ok(())
}

async fn complete_task_success(s: &mut EngineState, task_id: &str, round_id: i64, started_at: i64) {
    if let Some(task) = s.tasks.iter_mut().find(|task| task.id == task_id) {
        task.status = task_status::SUCCESS.to_string();
        task.assigned_device = None;
        clear_execution_cursor(task);
    }
    let _ = s.running.remove(task_id);
    s.storage.finish_round(round_id, round_status::COMPLETED).await;
    s.storage.finish_task_run(task_id, started_at, run_status::COMPLETED).await;
    s.storage
        .save_task_state(task_id, task_status::SUCCESS, None, None, None, None, 0, None, None, None)
        .await;
}

/// 一轮完成 + interval_minute > 0：重置进度、创建新轮次、注册延时唤醒
///
/// 内存安全：仅修改 task.cities 内的字段（原地覆写），不扩容 Vec
/// 性能：单次 DB 写入 clear_task_progress + create_round，无 N+1
async fn complete_round_with_interval(
    s: &mut EngineState,
    task_id: &str,
    round_id: i64,
    started_at: i64,
    interval_minutes: i32,
) {
    // 1. 结束当前轮次和 run
    s.storage.finish_round(round_id, round_status::COMPLETED).await;
    s.storage.finish_task_run(task_id, started_at, run_status::COMPLETED).await;

    // 2. 原地重置所有城市/关键词进度为 pending（零分配）
    if let Some(task) = s.tasks.iter_mut().find(|t| t.id == task_id) {
        for city in &mut task.cities {
            city.status = city_status::PENDING.to_string();
            city.progress = 0;
            city.done = 0;
            for keyword in &mut city.keywords {
                keyword.status = keyword_status::PENDING.to_string();
            }
        }
        clear_execution_cursor(task);
        // status 保持 EXECUTING — 设备不释放
    }

    // 3. 清除 DB 中本轮的关键词进度（新轮次从零开始）
    s.storage.clear_task_progress(task_id).await;

    // 4. 创建新轮次
    let new_round_id = s.storage.create_round(task_id).await.unwrap_or(0);
    if new_round_id == 0 {
        eprintln!("[engine] 创建新轮次失败: task={}, 标记为 error", task_id);
        mark_task_error(
            s,
            task_id,
            "创建新轮次失败（数据库错误）".to_string(),
            false,
            run_status::STOPPED,
        )
        .await;
        return;
    }

    // 5. 更新 runtime：新轮次 round_id，重置 attempt
    if let Some(runtime) = s.running.get_mut(task_id) {
        runtime.round_id = new_round_id;
        runtime.attempt = 0;
        runtime.last_error = None;
        // started_at 保留原值（设备连续占用），下次唤醒时 start_task_run 会更新
    }

    // 6. 注册延时 wakeup — interval_minutes 转毫秒
    // 防御性上限：最大 24 小时，避免整数溢出
    let clamped_minutes = interval_minutes.max(1).min(1440) as u64;
    let delay_ms = clamped_minutes * 60 * 1000;
    schedule_task(s, task_id, delay_ms);

    // 7. 持久化 — runtime_status 将被 persist_runtime_state 推断为 "interval_waiting"
    persist_runtime_state(s, task_id).await;

    eprintln!(
        "[engine] task={} 第 {} 轮完成，等待 {} 分钟后开始下一轮 (round_id={})",
        task_id,
        s.tasks.iter().find(|t| t.id == task_id).map(|t| t.round_no).unwrap_or(0),
        interval_minutes,
        new_round_id,
    );
}

async fn mark_task_error(
    s: &mut EngineState,
    task_id: &str,
    error_message: String,
    flag_device: bool,
    run_finish_status: &str,
) {
    let runtime = remove_runtime(s, task_id);
    let (round_id, started_at, device_serial, attempt) = runtime
        .as_ref()
        .map(|run| {
            (Some(run.round_id), Some(run.started_at), Some(run.device_serial.clone()), run.attempt)
        })
        .unwrap_or((None, None, None, 0));

    if let Some(round_id) = round_id {
        s.storage.finish_round(round_id, round_status::STOPPED).await;
    }
    if let (Some(started_at), Some(device_serial)) = (started_at, device_serial.as_deref()) {
        let _ = device_serial;
        s.storage.finish_task_run(task_id, started_at, run_finish_status).await;
    }

    if let Some(task) = s.tasks.iter_mut().find(|task| task.id == task_id) {
        rollback_running_keywords(task);
        task.status = task_status::ERROR.to_string();
        task.assigned_device = None;
        let _ = ensure_execution_cursor(task, false);
        let cursor = task_state_cursor(task);
        s.storage
            .save_task_state(
                task_id,
                task_status::ERROR,
                None,
                round_id,
                cursor.0,
                cursor.1,
                attempt,
                None,
                Some(error_message.as_str()),
                Some("error"),
            )
            .await;
    }

    if flag_device {
        if let Some(serial) = device_serial.as_deref() {
            s.storage.flag_device(serial).await;
            // 风控震动警告：10次短震（异步 fire-and-forget，不阻塞引擎）
            crate::connection::adb::vibrate_device_alert(serial);
            let _ = s.app_handle.emit(
                constants::tauri_event::RISK_CONTROL,
                serde_json::json!({
                    "task_id": task_id,
                    "device_serial": serial,
                    "message": error_message
                }),
            );
            let _ = s.app_handle.emit(constants::tauri_event::DEVICES_CHANGED, ());
        }
    }
}

async fn handle_device_kick(s: &mut EngineState, hw_serials: Vec<String>) -> u32 {
    let mut kicked = 0u32;

    for hw_serial in &hw_serials {
        let Some(device) = s.storage.get_device_by_hw_serial(hw_serial).await else {
            continue;
        };
        let serial = device.serial.clone();

        let task_to_pause = s
            .tasks
            .iter()
            .find(|task| {
                task.assigned_device.as_deref() == Some(&serial)
                    && (task.status == task_status::EXECUTING || task.status == task_status::PAUSED)
            })
            .map(|task| task.id.clone());

        if let Some(task_id) = task_to_pause {
            let _ = handle_pause(s, &task_id).await;
        }

        s.storage.delete_device(&serial).await;
        kicked += 1;
    }

    if kicked > 0 {
        let _ = s.app_handle.emit(constants::tauri_event::DEVICES_CHANGED, ());
    }
    kicked
}

async fn handle_phones_unbind(s: &mut EngineState, phones: Vec<String>) -> u32 {
    let mut all_task_ids = Vec::new();
    for phone in &phones {
        all_task_ids.extend(s.storage.get_tasks_by_phone(phone).await);
    }
    all_task_ids.sort();
    all_task_ids.dedup();
    if all_task_ids.is_empty() {
        return 0;
    }

    for task_id in &all_task_ids {
        if let Some(runtime) = remove_runtime(s, task_id) {
            s.storage.finish_round(runtime.round_id, round_status::STOPPED).await;
            s.storage
                .finish_task_run(task_id, runtime.started_at, run_status::STOPPED)
                .await;
        }
    }

    let id_set: HashSet<String> = all_task_ids.iter().cloned().collect();
    s.tasks.retain(|task| !id_set.contains(&task.id));
    s.storage.batch_cleanup_tasks(&all_task_ids).await;
    all_task_ids.len() as u32
}

async fn handle_task_reload_msg(s: &mut EngineState, action: &str, task_id: Option<&str>) {
    match action {
        "reload_all" => {
            handle_reload_tasks(s).await;
            force_emit(s).await;
        },
        "reload_task" => {
            if let Some(task_id) = task_id {
                if s.reloading.contains(task_id) {
                    return;
                }
                s.reloading.insert(task_id.to_string());
                merge_single_task(s, task_id).await;
                force_emit(s).await;
                s.reloading.remove(task_id);
            }
        },
        "delete_task" => {
            if let Some(task_id) = task_id {
                let is_running = s.tasks.iter().any(|task| {
                    task.id == task_id
                        && (task.status == task_status::EXECUTING
                            || task.status == task_status::PAUSED
                            || task.status == task_status::ERROR)
                });
                if is_running {
                    let _ = handle_stop(s, task_id).await;
                }
                s.tasks.retain(|task| task.id != task_id);
                s.storage.batch_cleanup_tasks(&[task_id.to_string()]).await;
                force_emit(s).await;
            }
        },
        _ => {},
    }
}

async fn merge_single_task(s: &mut EngineState, task_id: &str) {
    let batch_items = match s
        .http
        .batch_fetch_tasks(&crate::http::BatchTasksRequest { task_ids: vec![task_id.to_string()] })
        .await
    {
        Ok(items) => items,
        Err(_) => return,
    };
    let Some(item) = batch_items.into_iter().find(|item| item.task_id == task_id) else {
        return;
    };
    let new_def = task_sync::batch_item_to_task_def(&item);

    let payload = match serde_json::to_string(&new_def) {
        Ok(payload) => payload,
        Err(_) => return,
    };
    s.storage
        .upsert_task_def(task_id, &new_def.name, &payload, 1, &item.mobile)
        .await;

    let was_success = s
        .tasks
        .iter()
        .any(|task| task.id == task_id && task.status == task_status::SUCCESS);
    if was_success {
        s.storage.clear_task_progress(task_id).await;
        s.storage.delete_task_state(task_id).await;
    }

    let merged = task_provider::build_task(&s.storage, new_def).await;
    if let Some(local) = s.tasks.iter_mut().find(|task| task.id == task_id) {
        let keep_runtime = local.status == task_status::EXECUTING
            || local.status == task_status::PAUSED
            || local.status == task_status::ERROR;

        if keep_runtime {
            let saved_status = local.status.clone();
            let saved_device = local.assigned_device.clone();
            let saved_city = local.current_city_name.clone();
            let saved_keyword = local.current_keyword_name.clone();
            let old_cities = std::mem::take(&mut local.cities);
            *local = merged;
            local.status = saved_status;
            local.assigned_device = saved_device;
            local.current_city_name = saved_city;
            local.current_keyword_name = saved_keyword;

            for city in &mut local.cities {
                if let Some(old_city) = old_cities.iter().find(|item| item.name == city.name) {
                    city.status = old_city.status.clone();
                    city.done = old_city.done;
                    city.progress = old_city.progress;
                    for keyword in &mut city.keywords {
                        if let Some(old_keyword) =
                            old_city.keywords.iter().find(|item| item.name == keyword.name)
                        {
                            keyword.status = old_keyword.status.clone();
                        }
                    }
                }
            }

            let _ = ensure_execution_cursor(local, local.status == task_status::EXECUTING);
        } else {
            *local = merged;
        }
    } else {
        s.tasks.push(merged);
    }

    let valid_pairs = s
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .map(|task| {
            task.cities
                .iter()
                .flat_map(|city| {
                    city.keywords
                        .iter()
                        .map(move |keyword| (city.name.clone(), keyword.name.clone()))
                })
                .collect()
        })
        .unwrap_or_default();
    s.storage.cleanup_orphan_progress(task_id, valid_pairs).await;
}

async fn handle_release_offline(s: &mut EngineState, online_serials: &[String]) -> u32 {
    let online_set: HashSet<&str> = online_serials.iter().map(|serial| serial.as_str()).collect();
    let task_ids: Vec<String> = s
        .tasks
        .iter()
        .filter(|task| {
            task.assigned_device.is_some()
                && task.status == task_status::EXECUTING
                && !online_set.contains(task.assigned_device.as_deref().unwrap_or(""))
        })
        .map(|task| task.id.clone())
        .collect();

    for task_id in &task_ids {
        mark_task_error(s, task_id, "设备离线，任务已暂停".to_string(), false, run_status::STOPPED)
            .await;
    }

    task_ids.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_provider::{TaskCity, TaskKeyword};

    fn make_task() -> Task {
        Task {
            id: "task-1".to_string(),
            name: "task".to_string(),
            status: task_status::WAITING.to_string(),
            assigned_device: None,
            cities: vec![
                TaskCity {
                    name: "Wuhan".to_string(),
                    poi: "poi".to_string(),
                    progress: 0,
                    total: 2,
                    done: 0,
                    status: city_status::PENDING.to_string(),
                    keywords: vec![
                        TaskKeyword {
                            name: "k1".to_string(),
                            status: keyword_status::PENDING.to_string(),
                        },
                        TaskKeyword {
                            name: "k2".to_string(),
                            status: keyword_status::PENDING.to_string(),
                        },
                    ],
                },
                TaskCity {
                    name: "Shanghai".to_string(),
                    poi: "poi".to_string(),
                    progress: 0,
                    total: 1,
                    done: 0,
                    status: city_status::PENDING.to_string(),
                    keywords: vec![TaskKeyword {
                        name: "k3".to_string(),
                        status: keyword_status::PENDING.to_string(),
                    }],
                },
            ],
            interval_minute: None,
            round_no: 0,
            current_round_id: None,
            current_city_name: None,
            current_keyword_name: None,
        }
    }

    #[test]
    fn ensure_cursor_picks_first_pending_keyword() {
        let mut task = make_task();
        let cursor = ensure_execution_cursor(&mut task, true);
        assert_eq!(cursor, Some((0, 0)));
        assert_eq!(task.current_city_name.as_deref(), Some("Wuhan"));
        assert_eq!(task.current_keyword_name.as_deref(), Some("k1"));
        assert_eq!(task.cities[0].keywords[0].status, keyword_status::RUN);
    }

    #[test]
    fn move_cursor_skips_completed_keywords() {
        let mut task = make_task();
        let _ = ensure_execution_cursor(&mut task, true);
        task.cities[0].keywords[0].status = keyword_status::OK.to_string();
        task.cities[0].done = 1;
        task.cities[0].progress = 50;

        let cursor = move_to_next_cursor(&mut task, true);
        assert_eq!(cursor, Some((0, 1)));
        assert_eq!(task.current_keyword_name.as_deref(), Some("k2"));
        assert_eq!(task.cities[0].keywords[1].status, keyword_status::RUN);
    }

    #[test]
    fn is_all_keywords_pending_fresh_task() {
        let task = make_task();
        assert!(is_all_keywords_pending(&task));
    }

    #[test]
    fn is_all_keywords_pending_with_done() {
        let mut task = make_task();
        task.cities[0].keywords[0].status = keyword_status::OK.to_string();
        assert!(!is_all_keywords_pending(&task));
    }

    #[test]
    fn is_all_keywords_pending_after_reset() {
        let mut task = make_task();
        // 模拟一轮完成后重置
        task.cities[0].keywords[0].status = keyword_status::OK.to_string();
        task.cities[0].keywords[1].status = keyword_status::OK.to_string();
        task.cities[1].keywords[0].status = keyword_status::OK.to_string();
        assert!(!is_all_keywords_pending(&task));

        // 重置所有关键词
        for city in &mut task.cities {
            for keyword in &mut city.keywords {
                keyword.status = keyword_status::PENDING.to_string();
            }
        }
        assert!(is_all_keywords_pending(&task));
    }
}
