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
use crate::storage::{Database, DeviceRow, SaveTaskStateParams};
use crate::task_provider::{self, summarize_task, Task, TaskSummary};
use crate::task_sync;

use crate::connection::{
    adb::{adb_forward_remove, adb_forward_setup},
    phone_client::BatchTask,
};

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
    /// PC 本地端口（经 ADB forward 映射到手机 7899）
    local_port: u16,
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
        // 只阻止正在执行的任务的设备；ERROR 任务通过 is_flagged 机制阻止调度
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

fn task_runtime_status(task: &Task, runtime: Option<&RunningTaskState>) -> Option<String> {
    if task.status == task_status::EXECUTING {
        runtime
            .map(|run| {
                if run.active_worker.is_some() {
                    "executing"
                } else if run.next_wakeup_at.is_some() && is_all_keywords_pending(task) {
                    "interval_waiting"
                } else if run.next_wakeup_at.is_some() {
                    "scheduled"
                } else {
                    "idle"
                }
            })
            .or(Some("scheduled"))
            .map(str::to_string)
    } else if task.status == task_status::PAUSED {
        task.runtime_status.clone().or(Some("paused".to_string()))
    } else {
        task.runtime_status.clone()
    }
}

fn task_next_round_at(task: &Task, runtime: Option<&RunningTaskState>) -> Option<i64> {
    let runtime_status = task_runtime_status(task, runtime);
    match runtime_status.as_deref() {
        Some("interval_waiting") => runtime.and_then(|run| wakeup_unix(run.next_wakeup_at)),
        Some("interval_paused") => task.next_round_at,
        _ => None,
    }
}

fn enrich_task_runtime(task: &mut Task, runtime: Option<&RunningTaskState>) {
    task.runtime_status = task_runtime_status(task, runtime);
    task.presentation_status =
        task_provider::derive_presentation_status(&task.status, task.runtime_status.as_deref());
    task.next_round_at = task_next_round_at(task, runtime);
}

fn enrich_summaries_runtime(s: &EngineState, summaries: &mut [TaskSummary]) {
    for summary in summaries.iter_mut() {
        let runtime = s.running.get(&summary.id);
        if let Some(task) = s.tasks.iter().find(|t| t.id == summary.id) {
            summary.runtime_status = task_runtime_status(task, runtime);
            summary.presentation_status = task_provider::derive_presentation_status(
                &task.status,
                summary.runtime_status.as_deref(),
            );
            summary.next_round_at = task_next_round_at(task, runtime);
        } else {
            summary.runtime_status = None;
            summary.presentation_status =
                task_provider::derive_presentation_status(&summary.status, None);
            summary.next_round_at = None;
        }
    }
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
        summary.active_city_progress.hash(&mut hasher);
        summary.active_city_done.hash(&mut hasher);
        summary.active_city_total.hash(&mut hasher);
        summary.current_city_name.hash(&mut hasher);
        summary.current_keyword_name.hash(&mut hasher);
        summary.runtime_status.hash(&mut hasher);
        summary.presentation_status.hash(&mut hasher);
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

async fn sync_task_round_no(s: &mut EngineState, task_id: &str, round_id: i64) {
    let round_no = s.storage.get_round_no(round_id).await.unwrap_or(0);
    if let Some(task) = s.tasks.iter_mut().find(|task| task.id == task_id) {
        task.round_no = round_no;
        task.current_round_id = Some(round_id);
    }
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
    let runtime_status = task_runtime_status(task, runtime);

    s.storage
        .save_task_state(SaveTaskStateParams {
            task_id,
            status: &task.status,
            assigned_device,
            current_round_id,
            current_city_name: cursor.0,
            current_keyword_name: cursor.1,
            attempt,
            next_wakeup_at,
            last_error,
            runtime_status: runtime_status.as_deref(),
        })
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
    let Some(runtime) = s.running.get(task_id) else {
        return;
    };
    if runtime.active_worker.is_some() {
        return;
    }

    // 从内存任务状态中构建本批次待扫描列表
    let pending_tasks = s
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .map(build_pending_batch)
        .unwrap_or_default();

    if pending_tasks.is_empty() {
        // 无待扫描项：可能所有关键词已完成，由引擎在下一次 handle_worker_result 时处理
        eprintln!("[engine] spawn_task_worker: task={} 无待扫描关键词，跳过", task_id);
        return;
    }

    let round_id = runtime.round_id;
    let local_port = runtime.local_port;
    let device_serial = runtime.device_serial.clone();
    let batch_id = format!("{}-{}", task_id, round_id);
    let max_pages = constants::phone_client::DEFAULT_MAX_PAGES;

    let worker_seq = s.next_worker_seq;
    s.next_worker_seq += 1;
    let cancel = CancellationToken::new();
    let handle = spawn_worker(
        task_id.to_string(),
        device_serial,
        round_id,
        local_port,
        pending_tasks,
        batch_id,
        max_pages,
        worker_seq,
        cancel.clone(),
        s.tx.clone(),
        Arc::clone(&s.storage),
    );

    if let Some(runtime) = s.running.get_mut(task_id) {
        runtime.active_worker = Some(WorkerInfo { cancel, handle, worker_seq });
        runtime.next_wakeup_at = None;
    }
}

/// 从任务内存状态中提取所有待扫描的城市 + 关键词。
///
/// - 排除已标记为 DONE 的城市
/// - 排除状态为 OK 的关键词（已完成）
/// - 城市内无待扫关键词时整个城市跳过
fn build_pending_batch(task: &crate::task_provider::Task) -> Vec<BatchTask> {
    task.cities
        .iter()
        .filter(|city| city.status != city_status::DONE)
        .filter_map(|city| {
            let pending_kws: Vec<String> = city
                .keywords
                .iter()
                .filter(|kw| kw.status != keyword_status::OK)
                .map(|kw| kw.name.clone())
                .collect();
            if pending_kws.is_empty() {
                None
            } else {
                Some(BatchTask {
                    city: city.name.clone(),
                    poi: city.poi.clone(),
                    keywords: pending_kws,
                })
            }
        })
        .collect()
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
                    force_emit(&mut s).await;
                    continue;
                }
            }
        } else {
            rx.recv().await
        };

        let Some(msg) = maybe_msg else { break };
        let mut should_force_emit = false;

        match msg {
            EngineMsg::StartTask { task_id, reply } => {
                let _ = reply.send(handle_start(&mut s, &task_id).await);
                should_force_emit = true;
            },
            EngineMsg::PauseTask { task_id, reply } => {
                let _ = reply.send(handle_pause(&mut s, &task_id).await);
                should_force_emit = true;
            },
            EngineMsg::ResumeTask { task_id, reply } => {
                let _ = reply.send(handle_resume(&mut s, &task_id).await);
                should_force_emit = true;
            },
            EngineMsg::StopTask { task_id, reply } => {
                let _ = reply.send(handle_stop(&mut s, &task_id).await);
                should_force_emit = true;
            },
            EngineMsg::RetryTask { task_id, reply } => {
                let _ = reply.send(handle_retry(&mut s, &task_id).await);
                should_force_emit = true;
            },
            EngineMsg::GetTasks { reply } => {
                let mut summaries = build_summaries(&s.tasks);
                enrich_summaries_runtime(&s, &mut summaries);
                let _ = reply.send(summaries);
            },
            EngineMsg::GetTaskDetail { task_id, reply } => {
                let detail =
                    s.tasks.iter().find(|task| task.id == task_id).cloned().map(|mut task| {
                        let runtime = s.running.get(&task.id);
                        enrich_task_runtime(&mut task, runtime);
                        task
                    });
                let _ = reply.send(detail);
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
                should_force_emit = true;
            },
            EngineMsg::ReloadTasks => {
                handle_reload_tasks(&mut s).await;
                should_force_emit = true;
            },
            EngineMsg::HandleTaskReload { action, task_id } => {
                handle_task_reload_msg(&mut s, &action, task_id.as_deref()).await;
                should_force_emit = true;
            },
            EngineMsg::HandleDeviceKick { hw_serials, reply } => {
                let _ = reply.send(handle_device_kick(&mut s, hw_serials).await);
                should_force_emit = true;
            },
            EngineMsg::HandlePhonesUnbind { phones, reply } => {
                let _ = reply.send(handle_phones_unbind(&mut s, phones).await);
                should_force_emit = true;
            },
            EngineMsg::ReleaseOfflineDevices { online_serials, reply } => {
                let _ = reply.send(handle_release_offline(&mut s, &online_serials).await);
                should_force_emit = true;
            },
            EngineMsg::ScanProgress { task_id, city, keyword, status, worker_seq } => {
                handle_scan_progress(&mut s, &task_id, &city, &keyword, &status, worker_seq);
                // Fix-P1：ScanProgress 是高频消息，使用节流 emit_update 代替 force_emit，
                // 避免每次状态变更都触发全量 summaries 序列化 + IPC 发送
            },
            EngineMsg::KeywordDone { task_id, city, keyword, worker_seq } => {
                handle_keyword_done(&mut s, &task_id, &city, &keyword, worker_seq);
                should_force_emit = true;
            },
            EngineMsg::WorkerResult { task_id, worker_seq, outcome } => {
                handle_worker_result(&mut s, &task_id, worker_seq, outcome).await;
                should_force_emit = true;
            },
            EngineMsg::ClearTaskDevice { task_id, reply } => {
                let result = handle_clear_task_device(&mut s, &task_id).await;
                let _ = reply.send(result);
            },
            EngineMsg::Shutdown { reply } => {
                // Fix-M2：收集 worker handle，等待它们完成 DB 写入后再退出
                let mut worker_handles = Vec::new();
                for (task_id, mut runtime) in s.running.drain() {
                    if let Some(worker) = runtime.active_worker.take() {
                        worker.cancel.cancel();
                        worker_handles.push(worker.handle);
                    }
                    let serial = runtime.device_serial.clone();
                    let port = runtime.local_port;
                    tokio::spawn(async move {
                        adb_forward_remove(&serial, port).await;
                    });
                    eprintln!("[engine] shutdown: cancelled runtime {}", task_id);
                }
                // 等待所有 worker 完成（最多 3 秒），确保正在进行的 DB 写入不被中断
                if !worker_handles.is_empty() {
                    let _ = tokio::time::timeout(Duration::from_secs(3), async {
                        for handle in worker_handles {
                            let _ = handle.await;
                        }
                    })
                    .await;
                }
                let _ = reply.send(());
                break;
            },
        }

        if should_force_emit {
            force_emit(&mut s).await;
        } else {
            emit_update(&mut s).await;
        }
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

    // Fix-P5：清理僵尸 wakeup（任务 pause/resume/retry 循环产生的过期条目）
    // 当堆大小远超活跃 runtime 数量时，重建堆以释放无效条目
    let threshold = s.running.len() * 3 + 10;
    if s.wakeups.len() > threshold {
        let valid: Vec<_> = s
            .wakeups
            .drain()
            .filter(|Reverse(w)| {
                s.running
                    .get(&w.task_id)
                    .map(|rt| rt.wakeup_seq == w.wakeup_seq)
                    .unwrap_or(false)
            })
            .collect();
        s.wakeups.extend(valid);
    }
}

async fn emit_update(s: &mut EngineState) {
    let throttle_ms = constants::debug::EMIT_THROTTLE_MS;
    if s.last_emit.elapsed() < Duration::from_millis(throttle_ms) {
        return;
    }

    let mut summaries = build_summaries(&s.tasks);
    enrich_summaries_runtime(s, &mut summaries);
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
    enrich_summaries_runtime(s, &mut summaries);
    s.last_hash = compute_summaries_hash(&summaries);
    let _ = s.snapshot_tx.send(summaries.clone());
    let snapshot = TaskSummarySnapshotRef { tasks: summaries.as_slice() };
    let _ = s.app_handle.emit(constants::tauri_event::TASK_UPDATE, &snapshot);
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

    // 分配（或复用）PC 本地端口，并建立 ADB forward
    let local_port = s.storage.assign_device_port(&serial).await;
    if let Err(e) = adb_forward_setup(&serial, local_port).await {
        eprintln!(
            "[engine] start: ADB forward 设置失败 (device={}, port={})，继续尝试（forward 可能已存在）: {}",
            serial, local_port, e
        );
    }

    // CON-1 修复：用 ok_or 替代 unwrap，避免 .await 点后内存状态变化导致意外 panic
    let task = s
        .tasks
        .iter_mut()
        .find(|task| task.id == task_id)
        .ok_or_else(|| format!("任务 {} 在启动中意外消失（内存不一致）", task_id))?;
    task.status = task_status::EXECUTING.to_string();
    task.runtime_status = Some("executing".to_string());
    task.next_round_at = None;
    task.assigned_device = Some(serial.clone());
    let _ = ensure_execution_cursor(task, true);

    let round_id = match s.storage.create_round(task_id).await {
        Some(round_id) => round_id,
        None => {
            // CON-1 修复：回滚路径也用 if let 更安全
            // Fix-L1：ensure_execution_cursor(mark_running=true) 已将关键词标为 RUN，
            // 回滚时必须同步 rollback，否则 WAITING 任务中残留 RUN 状态关键词
            if let Some(task) = s.tasks.iter_mut().find(|task| task.id == task_id) {
                rollback_running_keywords(task);
                task.status = task_status::WAITING.to_string();
                task.assigned_device = None;
                clear_execution_cursor(task);
            }
            return Err("创建轮次失败（数据库错误），无法启动任务".into());
        },
    };
    sync_task_round_no(s, task_id, round_id).await;

    // LOG-2 修复：start_task_run 失败时回滚内存状态，避免 DB/内存不一致
    let started_at = s.storage.start_task_run(task_id, &serial, round_id).await;
    s.running.insert(
        task_id.to_string(),
        RunningTaskState {
            round_id,
            started_at,
            device_serial: serial,
            local_port,
            active_worker: None,
            next_wakeup_at: None,
            wakeup_seq: 0,
            attempt: 0,
            last_error: None,
        },
    );
    spawn_task_worker(s, task_id);
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

    // 暂停前检测是否处于 interval_waiting（保留 next_wakeup_at 供 resume 使用）
    let was_interval_waiting = s
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .map(|task| task.interval_minute.unwrap_or(0) > 0 && is_all_keywords_pending(task))
        .unwrap_or(false);

    let mut runtime = s.running.remove(task_id);
    if let Some(run) = runtime.as_mut() {
        if let Some(worker) = run.active_worker.take() {
            worker.cancel.cancel();
        }
    }
    let round_id = runtime.as_ref().map(|run| run.round_id);
    let device_forward = runtime.as_ref().map(|r| (r.device_serial.clone(), r.local_port));

    // interval_waiting 暂停时，将 next_wakeup_at 转为 Unix 时间戳保存
    // 以便 resume 时计算剩余等待时间
    let saved_wakeup = if was_interval_waiting {
        runtime.as_ref().and_then(|run| wakeup_unix(run.next_wakeup_at))
    } else {
        None
    };

    if let Some(run) = runtime.as_mut() {
        clear_task_schedule(run);
    }

    if let Some(run) = runtime {
        s.storage.finish_round(run.round_id, round_status::STOPPED).await;
        s.storage.finish_task_run(task_id, run.started_at, run_status::PAUSED).await;
    }

    // Fix-A1：释放 ADB port-forward 改为非阻塞，避免 ADB 进程挂起时阻塞事件循环
    if let Some((serial, port)) = device_forward {
        tokio::spawn(async move {
            adb_forward_remove(&serial, port).await;
        });
    }

    let runtime_status_label = if was_interval_waiting {
        "interval_paused" // 标记是从 interval_waiting 暂停的
    } else {
        "paused"
    };
    if let Some(task) = s.tasks.iter_mut().find(|task| task.id == task_id) {
        rollback_running_keywords(task);
        task.status = task_status::PAUSED.to_string();
        task.runtime_status = Some(runtime_status_label.to_string());
        task.next_round_at = saved_wakeup;
        task.assigned_device = None;
        let _ = ensure_execution_cursor(task, false);
    }

    // CON-1 修复：用 if let 替代 unwrap
    if let Some(task) = s.tasks.iter().find(|task| task.id == task_id) {
        let cursor = task_state_cursor(task);
        s.storage
            .save_task_state(SaveTaskStateParams {
                task_id,
                status: task_status::PAUSED,
                current_round_id: round_id,
                current_city_name: cursor.0,
                current_keyword_name: cursor.1,
                next_wakeup_at: saved_wakeup,
                runtime_status: Some(runtime_status_label),
                ..Default::default()
            })
            .await;
    }
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

    // 复用已分配端口（或首次分配），重建 ADB forward（设备重连后 forward 可能已失效）
    let local_port = s.storage.assign_device_port(&serial).await;
    if let Err(e) = adb_forward_setup(&serial, local_port).await {
        eprintln!(
            "[engine] resume: ADB forward 设置失败 (device={}, port={}): {}",
            serial, local_port, e
        );
    }

    let saved_state = s.storage.load_task_state(task_id).await;

    // 检测是否从 interval_waiting 恢复（暂停时标记了 interval_paused 并保留了 next_wakeup_at）
    let was_interval_paused = saved_state
        .as_ref()
        .map(|st| st.runtime_status.as_deref() == Some("interval_paused"))
        .unwrap_or(false);
    let saved_wakeup_at = saved_state.as_ref().and_then(|st| st.next_wakeup_at);

    // 先计算恢复策略：是否还需要继续等待
    let still_waiting =
        was_interval_paused && saved_wakeup_at.map(|t| t > constants::now_unix()).unwrap_or(false);

    // CON-1 修复：用 ok_or_else 替代 unwrap
    let task = s
        .tasks
        .iter_mut()
        .find(|task| task.id == task_id)
        .ok_or_else(|| format!("任务 {} 在恢复过程中意外消失", task_id))?;
    rollback_running_keywords(task);
    task.status = task_status::EXECUTING.to_string();
    task.runtime_status =
        Some(if still_waiting { "interval_waiting" } else { "executing" }.to_string());
    task.next_round_at = if still_waiting { saved_wakeup_at } else { None };
    task.assigned_device = Some(serial.clone());
    let _ = ensure_execution_cursor(task, !still_waiting);

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
    sync_task_round_no(s, task_id, round_id).await;

    let started_at = s.storage.start_task_run(task_id, &serial, round_id).await;
    s.running.insert(
        task_id.to_string(),
        RunningTaskState {
            round_id,
            started_at,
            device_serial: serial,
            local_port,
            active_worker: None,
            next_wakeup_at: None,
            wakeup_seq: 0,
            attempt: saved_state.as_ref().map(|state| state.attempt).unwrap_or(0),
            last_error: saved_state.and_then(|state| state.last_error),
        },
    );

    // 智能恢复策略：从 interval_waiting 暂停的任务
    if still_waiting {
        let remaining_secs = (saved_wakeup_at.unwrap() - constants::now_unix()) as u64;
        eprintln!("[engine] interval_paused 恢复: task={}, 剩余等待 {}s", task_id, remaining_secs);
        schedule_task(s, task_id, remaining_secs * 1000);
    } else {
        if was_interval_paused {
            eprintln!("[engine] interval_paused 恢复: task={}, 等待已过期, 立即执行", task_id);
        }
        spawn_task_worker(s, task_id);
    }
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

    // 复用已分配端口，重建 ADB forward
    let local_port = s.storage.assign_device_port(&serial).await;
    if let Err(e) = adb_forward_setup(&serial, local_port).await {
        eprintln!(
            "[engine] retry: ADB forward 设置失败 (device={}, port={}): {}",
            serial, local_port, e
        );
    }

    if let Some(mut fresh) = fresh {
        fresh.status = task_status::EXECUTING.to_string();
        fresh.runtime_status = Some("executing".to_string());
        fresh.next_round_at = None;
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
    sync_task_round_no(s, task_id, round_id).await;
    let started_at = s.storage.start_task_run(task_id, &serial, round_id).await;

    s.running.insert(
        task_id.to_string(),
        RunningTaskState {
            round_id,
            started_at,
            device_serial: serial,
            local_port,
            active_worker: None,
            next_wakeup_at: None,
            wakeup_seq: 0,
            attempt: 0,
            last_error: None,
        },
    );
    spawn_task_worker(s, task_id);
    persist_runtime_state(s, task_id).await;
    Ok(())
}

async fn cancel_and_cleanup(s: &mut EngineState, task_id: &str) -> Result<Option<Task>, String> {
    if !s.tasks.iter().any(|task| task.id == task_id) {
        return Err("任务不存在".into());
    }

    let device_forward = if let Some(runtime) = remove_runtime(s, task_id) {
        let info = (runtime.device_serial.clone(), runtime.local_port);
        s.storage.finish_round(runtime.round_id, round_status::STOPPED).await;
        s.storage
            .finish_task_run(task_id, runtime.started_at, run_status::STOPPED)
            .await;
        Some(info)
    } else {
        None
    };

    s.storage.clear_task_progress(task_id).await;
    s.storage.delete_task_state(task_id).await;

    // Fix-A1：非阻塞释放 ADB port-forward
    if let Some((serial, port)) = device_forward {
        tokio::spawn(async move {
            adb_forward_remove(&serial, port).await;
        });
    }

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
    /// (status, assigned_device, current_city, current_keyword)
    type RunSnapshot = (String, Option<String>, Option<String>, Option<String>);
    let running_snapshot: HashMap<String, RunSnapshot> = s
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
        ExecutionOutcome::Cancelled => {
            // 取消由 handle_pause / handle_stop 触发，状态已由它们更新，此处无需额外处理
        },
        ExecutionOutcome::DeviceOffline => {
            // flag_device=true：设备重新上线后也不会被自动调度，需用户手动解除异常标记
            mark_task_error(
                s,
                task_id,
                "设备离线，任务已暂停".to_string(),
                true,
                run_status::STOPPED,
            )
            .await;
        },
        ExecutionOutcome::BatchDone { completed, stopped } => {
            if handle_batch_done(s, task_id, completed, stopped).await.is_err() {
                mark_task_error(
                    s,
                    task_id,
                    "批次结果处理异常".to_string(),
                    true,
                    run_status::STOPPED,
                )
                .await;
            }
        },
        ExecutionOutcome::FatalError { completed, city, reason } => {
            // 先保存已完成进度
            if !completed.is_empty() {
                if let Some(task) = s.tasks.iter_mut().find(|t| t.id == task_id) {
                    apply_completed_to_task(task, &completed);
                }
            }
            // 停止任务并标记设备（flag_device=true），使设备不可调度直到用户手动解除
            let error_msg = format!("致命错误 [{}]: {}", city, reason);
            mark_task_error(s, task_id, error_msg, true, run_status::STOPPED).await;
            // 通知前端
            let _ = s.app_handle.emit(
                "task://fatal-error",
                serde_json::json!({ "task_id": task_id, "city": city, "reason": reason }),
            );
        },
    }
}

/// 实时处理手机端进度通知（切换城市 / 开始扫描关键词）。
///
/// - `switching_location`: 更新当前城市，将目标城市标为 ACTIVE，其余 PENDING
/// - `searching`: 更新当前关键词，将目标关键词标为 RUN
fn handle_scan_progress(
    s: &mut EngineState,
    task_id: &str,
    city: &str,
    keyword: &str,
    status: &str,
    worker_seq: u64,
) {
    // 过期 worker 的消息直接丢弃
    let active_seq = s
        .running
        .get(task_id)
        .and_then(|r| r.active_worker.as_ref())
        .map(|w| w.worker_seq);
    if active_seq != Some(worker_seq) {
        return;
    }

    let Some(task) = s.tasks.iter_mut().find(|t| t.id == task_id) else { return };

    match status {
        "switching_location" => {
            task.current_city_name = Some(city.to_string());
            task.current_keyword_name = None;
            // 将目标城市标为 ACTIVE，其余非 DONE 城市还原为 PENDING
            for c in &mut task.cities {
                if c.status == city_status::DONE {
                    continue;
                }
                c.status = if c.name == city {
                    city_status::ACTIVE.to_string()
                } else {
                    city_status::PENDING.to_string()
                };
            }
        },
        "searching" => {
            task.current_city_name = Some(city.to_string());
            task.current_keyword_name = Some(keyword.to_string());
            // 将目标关键词标为 RUN（同城市内其他 non-OK 关键词保持 PENDING）
            if let Some(city_entry) = task.cities.iter_mut().find(|c| c.name == city) {
                for kw in &mut city_entry.keywords {
                    if kw.status == keyword_status::OK {
                        continue;
                    }
                    kw.status = if kw.name == keyword {
                        keyword_status::RUN.to_string()
                    } else {
                        keyword_status::PENDING.to_string()
                    };
                }
            }
        },
        _ => {},
    }
}

/// 实时处理单个关键词完成通知（来自 worker 的流式进度推送）。
///
/// 立即将该关键词应用到内存任务状态，触发前端实时更新。
/// `worker_seq` 用于防止过期 worker 的消息干扰引擎状态（任务重启后旧 worker 延迟消息）。
fn handle_keyword_done(
    s: &mut EngineState,
    task_id: &str,
    city: &str,
    keyword: &str,
    worker_seq: u64,
) {
    // 校验 worker_seq：过期 worker 的消息直接丢弃
    let active_seq = s
        .running
        .get(task_id)
        .and_then(|r| r.active_worker.as_ref())
        .map(|w| w.worker_seq);
    if active_seq != Some(worker_seq) {
        return;
    }

    if let Some(task) = s.tasks.iter_mut().find(|t| t.id == task_id) {
        apply_completed_to_task(task, &[(city.to_string(), keyword.to_string())]);
    }
}

/// 处理手机端批次完成（BatchDone）结果。
///
/// ## 流程
/// 1. 将已完成的 (city, keyword) 列表应用到内存任务状态
/// 2. 检查整体完成度
///    - **全部完成**：调用 `complete_task_success` 或 `complete_round_with_interval`
///    - **部分完成**（手机端提前停止 / 意外情况）：调度下一轮 Worker 继续扫描剩余关键词
async fn handle_batch_done(
    s: &mut EngineState,
    task_id: &str,
    completed: Vec<(String, String)>,
    stopped: bool,
) -> Result<(), String> {
    let (round_id, started_at) = {
        let runtime = s.running.get(task_id).ok_or("运行时不存在")?;
        (runtime.round_id, runtime.started_at)
    };

    let task = s.tasks.iter_mut().find(|task| task.id == task_id).ok_or("任务不存在")?;
    if task.status != task_status::EXECUTING {
        return Ok(());
    }

    // ── Mock 风控（调试用，正式版 MOCK_RISK_ENABLED = false）──
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

    // ── 将本批次完成的关键词应用到内存任务状态 ──
    apply_completed_to_task(task, &completed);

    // ── 更新游标（不标记 run，因为批次可能仍有剩余）──
    let _ = ensure_execution_cursor(task, false);

    // ── 检查整体完成度 ──
    let all_done = task
        .cities
        .iter()
        .all(|city| city.keywords.iter().all(|kw| kw.status == keyword_status::OK));

    if all_done {
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
    } else {
        // 部分完成：手机端提前停止或意外断开 — 调度下一轮继续扫描剩余关键词
        if stopped {
            eprintln!(
                "[engine] task={} 手机端提前停止，已完成 {}/{} 关键词，调度继续",
                task_id,
                completed.len(),
                s.tasks
                    .iter()
                    .find(|t| t.id == task_id)
                    .map(|t| t.cities.iter().map(|c| c.total).sum::<i32>())
                    .unwrap_or(0),
            );
        } else {
            eprintln!("[engine] task={} 批次未完全执行（未收到 done），调度继续", task_id);
        }

        if let Some(runtime) = s.running.get_mut(task_id) {
            runtime.attempt = 0;
            runtime.last_error = None;
        }
        schedule_task(s, task_id, constants::timing::TASK_DISPATCH_INTERVAL_SECS * 1000);
        persist_runtime_state(s, task_id).await;
    }

    Ok(())
}

/// 将批次完成的 (city, keyword) 列表应用到内存任务状态。
///
/// - 对每个已完成的关键词：将状态置为 OK，更新城市 done 计数和进度
/// - 若城市内所有关键词均已完成，将城市状态置为 DONE
/// - **幂等**：关键词已是 OK 状态时跳过（不重复计数）
fn apply_completed_to_task(task: &mut crate::task_provider::Task, completed: &[(String, String)]) {
    for (city_name, keyword_name) in completed {
        let Some(city) = task.cities.iter_mut().find(|c| c.name == *city_name) else {
            eprintln!(
                "[engine] apply_completed: 城市 '{}' 不存在于任务 '{}'，跳过",
                city_name, task.id
            );
            continue;
        };

        let newly_done = city
            .keywords
            .iter_mut()
            .find(|kw| kw.name == *keyword_name && kw.status != keyword_status::OK)
            .map(|kw| {
                kw.status = keyword_status::OK.to_string();
                true
            })
            .unwrap_or(false);

        if newly_done {
            city.done += 1;
            city.progress = if city.total > 0 {
                ((city.done as f64 / city.total as f64) * 100.0).round() as i32
            } else {
                100
            };
        }
    }

    // 城市完成度检查：所有关键词 OK → 城市状态 DONE
    for city in &mut task.cities {
        if city.status != city_status::DONE
            && city.keywords.iter().all(|kw| kw.status == keyword_status::OK)
        {
            city.status = city_status::DONE.to_string();
            city.progress = 100;
            city.done = city.total; // 修正计数（防止浮点舍入导致 done < total）
        }
    }
}

async fn complete_task_success(s: &mut EngineState, task_id: &str, round_id: i64, started_at: i64) {
    if let Some(task) = s.tasks.iter_mut().find(|task| task.id == task_id) {
        task.status = task_status::SUCCESS.to_string();
        task.runtime_status = None;
        task.next_round_at = None;
        task.assigned_device = None;
        clear_execution_cursor(task);
    }
    let _ = s.running.remove(task_id);
    s.storage.finish_round(round_id, round_status::COMPLETED).await;
    s.storage.finish_task_run(task_id, started_at, run_status::COMPLETED).await;
    s.storage
        .save_task_state(SaveTaskStateParams {
            task_id,
            status: task_status::SUCCESS,
            ..Default::default()
        })
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
            true,
            run_status::STOPPED,
        )
        .await;
        return;
    }
    sync_task_round_no(s, task_id, new_round_id).await;

    // 5. 更新 runtime：新轮次 round_id，重置 attempt
    if let Some(runtime) = s.running.get_mut(task_id) {
        runtime.round_id = new_round_id;
        runtime.attempt = 0;
        runtime.last_error = None;
        // started_at 保留原值（设备连续占用），下次唤醒时 start_task_run 会更新
    }

    // 6. 注册延时 wakeup — interval_minutes 转毫秒
    // 防御性上限：最大 24 小时，避免整数溢出
    let clamped_minutes = interval_minutes.clamp(1, 1440) as u64;
    let delay_ms = clamped_minutes * 60 * 1000;
    schedule_task(s, task_id, delay_ms);
    if let Some(task) = s.tasks.iter_mut().find(|t| t.id == task_id) {
        task.runtime_status = Some("interval_waiting".to_string());
        task.next_round_at =
            s.running.get(task_id).and_then(|runtime| wakeup_unix(runtime.next_wakeup_at));
    }

    // 7. 持久化 — runtime_status 将被 persist_runtime_state 推断为 "interval_waiting"
    persist_runtime_state(s, task_id).await;

    eprintln!(
        "[engine] task={} 第 {} 轮完成，等待 {} 分钟后开始下一轮 (round_id={})",
        task_id,
        s.tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.round_no.saturating_sub(1))
            .unwrap_or(0),
        interval_minutes,
        new_round_id,
    );
}

/// 清除 ERROR 任务的 assigned_device，使设备重新可调度。
/// 任务状态保持 ERROR（用户仍可 resume/retry），仅解除设备锁定。
async fn handle_clear_task_device(s: &mut EngineState, task_id: &str) -> Result<(), String> {
    let task = s
        .tasks
        .iter_mut()
        .find(|t| t.id == task_id && t.status == task_status::ERROR)
        .ok_or_else(|| "任务不处于错误状态，无法清除设备关联".to_string())?;

    task.assigned_device = None;

    // 持久化（只更新 assigned_device，其余字段用 Default 保留 null）
    let cursor = task_state_cursor(task);
    let attempt = task.runtime_status.as_deref().map(|_| 0).unwrap_or(0);
    s.storage
        .save_task_state(SaveTaskStateParams {
            task_id,
            status: task_status::ERROR,
            assigned_device: None,
            current_city_name: cursor.0,
            current_keyword_name: cursor.1,
            runtime_status: Some("error"),
            attempt,
            ..Default::default()
        })
        .await;

    Ok(())
}

async fn mark_task_error(
    s: &mut EngineState,
    task_id: &str,
    error_message: String,
    flag_device: bool,
    run_finish_status: &str,
) {
    let runtime = remove_runtime(s, task_id);
    let (round_id, started_at, device_serial, local_port, attempt) = runtime
        .as_ref()
        .map(|run| {
            (
                Some(run.round_id),
                Some(run.started_at),
                Some(run.device_serial.clone()),
                Some(run.local_port),
                run.attempt,
            )
        })
        .unwrap_or((None, None, None, None, 0));

    if let Some(round_id) = round_id {
        s.storage.finish_round(round_id, round_status::STOPPED).await;
    }
    if let Some(started_at) = started_at {
        s.storage.finish_task_run(task_id, started_at, run_finish_status).await;
    }
    // Fix-A1：非阻塞释放 ADB port-forward，避免 ADB 进程挂起时阻塞事件循环
    if let (Some(serial), Some(port)) = (device_serial.clone(), local_port) {
        tokio::spawn(async move {
            adb_forward_remove(&serial, port).await;
        });
    }

    if let Some(task) = s.tasks.iter_mut().find(|task| task.id == task_id) {
        rollback_running_keywords(task);
        task.status = task_status::ERROR.to_string();
        task.runtime_status = Some("error".to_string());
        task.next_round_at = None;
        // 保留 assigned_device（仅用于前端设备卡片显示异常任务来源）
        // 调度阻止通过 is_flagged（FatalError 路径）或设备 OFFLINE 状态实现
        task.assigned_device = device_serial.clone();
        let _ = ensure_execution_cursor(task, false);
        let cursor = task_state_cursor(task);
        s.storage
            .save_task_state(SaveTaskStateParams {
                task_id,
                status: task_status::ERROR,
                assigned_device: device_serial.as_deref(),
                current_round_id: round_id,
                current_city_name: cursor.0,
                current_keyword_name: cursor.1,
                attempt,
                last_error: Some(error_message.as_str()),
                runtime_status: Some("error"),
                ..Default::default()
            })
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
            runtime_status: None,
            presentation_status: crate::constants::task_presentation_status::READY.to_string(),
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
            next_round_at: None,
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
