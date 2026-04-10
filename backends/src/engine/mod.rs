mod event_loop;
mod worker;

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};

use crate::http;
use crate::storage::Database;
use crate::task_provider::{self, Task, TaskSummary};

// ─── 公共类型 ──────────────────────────────────────────

/// 零拷贝快照：序列化时直接引用 tasks
#[derive(Serialize)]
pub(crate) struct TaskSummarySnapshotRef<'a> {
    pub tasks: &'a [TaskSummary],
}

// ─── 消息类型 ──────────────────────────────────────────

pub(crate) enum EngineMsg {
    // ── 任务生命周期 ──
    StartTask {
        task_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    PauseTask {
        task_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ResumeTask {
        task_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    StopTask {
        task_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RetryTask {
        task_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },

    // ── 查询 ──
    GetTasks {
        reply: oneshot::Sender<Vec<TaskSummary>>,
    },
    GetTaskDetail {
        task_id: String,
        reply: oneshot::Sender<Option<Task>>,
    },
    GetReadySerials {
        reply: oneshot::Sender<Vec<String>>,
    },

    // ── 状态管理 ──
    ReorderCities {
        task_id: String,
        new_order: Vec<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ReloadTasks,

    // ── MQTT 处理 ──
    HandleTaskReload {
        action: String,
        task_id: Option<String>,
    },
    HandleDeviceKick {
        hw_serials: Vec<String>,
        reply: oneshot::Sender<u32>,
    },
    HandlePhonesUnbind {
        phones: Vec<String>,
        reply: oneshot::Sender<u32>,
    },
    ReleaseOfflineDevices {
        online_serials: Vec<String>,
        reply: oneshot::Sender<u32>,
    },

    // ── Worker 回报 ──
    /// 手机端切换城市 / 开始扫描关键词（实时位置同步）
    ScanProgress {
        task_id: String,
        city: String,
        keyword: String,
        /// "switching_location" | "searching"
        status: String,
        worker_seq: u64,
    },
    /// 单个关键词完成（实时进度，已写入 DB）
    KeywordDone {
        task_id: String,
        city: String,
        keyword: String,
        worker_seq: u64,
    },
    WorkerResult {
        task_id: String,
        worker_seq: u64,
        outcome: ExecutionOutcome,
    },
    /// 清除 ERROR 任务的设备关联，使设备重新可调度（用户手动确认异常后操作）
    ClearTaskDevice {
        task_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// P1 修复：优雅关闭，取消所有 worker
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Debug)]
pub(crate) enum ExecutionOutcome {
    /// 手机端返回了 `done` 消息（或部分完成后连接关闭）。
    ///
    /// - `completed` — 本批次中已确认完成的 (city, keyword) 列表（已写入 DB）
    /// - `stopped` — 手机端是否提前停止（true 时可能仍有未完成的关键词）
    BatchDone {
        completed: Vec<(String, String)>,
        stopped: bool,
    },
    /// TCP 连接失败或流中断，且无任何已完成结果（判定设备离线）
    DeviceOffline,
    /// 被 CancellationToken 取消（用户暂停 / 停止任务）
    Cancelled,
    /// 手机端报告不可恢复的致命错误（如切换城市超时），任务终止并释放设备
    FatalError {
        completed: Vec<(String, String)>,
        city: String,
        reason: String,
    },
}

// ─── TaskEngine（thin sender wrapper）─────────────────

const ENGINE_CHANNEL_SIZE: usize = 256;

pub struct TaskEngine {
    tx: mpsc::Sender<EngineMsg>,
    snapshot_rx: watch::Receiver<Vec<TaskSummary>>,
}

impl TaskEngine {
    /// 初始化引擎：加载任务 → 启动事件循环 → 返回 handle
    pub async fn new(
        storage: Arc<Database>,
        http: Arc<dyn http::ApiClient>,
        app_handle: tauri::AppHandle,
    ) -> Arc<Self> {
        let tasks = task_provider::load_tasks(&storage).await;
        let summaries = tasks.iter().map(task_provider::summarize_task).collect::<Vec<_>>();
        let (tx, rx) = mpsc::channel(ENGINE_CHANNEL_SIZE);
        let (snapshot_tx, snapshot_rx) = watch::channel(summaries);

        // 启动事件循环
        event_loop::spawn(rx, tx.clone(), snapshot_tx, tasks, storage, http, app_handle);

        Arc::new(Self { tx, snapshot_rx })
    }

    // ── 辅助：发送消息并等待回复 ──

    async fn send_and_recv<T>(
        &self,
        msg_fn: impl FnOnce(oneshot::Sender<T>) -> EngineMsg,
    ) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx.send(msg_fn(reply_tx)).await.map_err(|_| "引擎已关闭".to_string())?;
        reply_rx.await.map_err(|_| "引擎响应丢失".to_string())
    }

    async fn send_fire_and_forget(&self, msg: EngineMsg) {
        let _ = self.tx.send(msg).await;
    }

    // ── 公共 API（与旧接口完全兼容）──

    pub async fn get_tasks(&self) -> Vec<TaskSummary> {
        self.send_and_recv(|reply| EngineMsg::GetTasks { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn get_task_detail(&self, task_id: &str) -> Option<Task> {
        self.send_and_recv(|reply| EngineMsg::GetTaskDetail { task_id: task_id.to_string(), reply })
            .await
            .ok()
            .flatten()
    }

    pub fn subscribe_tasks(&self) -> watch::Receiver<Vec<TaskSummary>> {
        self.snapshot_rx.clone()
    }

    pub async fn start_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::StartTask { task_id: task_id.to_string(), reply })
            .await?
    }

    pub async fn pause_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::PauseTask { task_id: task_id.to_string(), reply })
            .await?
    }

    pub async fn resume_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::ResumeTask { task_id: task_id.to_string(), reply })
            .await?
    }

    pub async fn stop_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::StopTask { task_id: task_id.to_string(), reply })
            .await?
    }

    pub async fn retry_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::RetryTask { task_id: task_id.to_string(), reply })
            .await?
    }

    pub async fn get_ready_serials(&self) -> Vec<String> {
        self.send_and_recv(|reply| EngineMsg::GetReadySerials { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn reorder_cities(
        &self,
        task_id: &str,
        new_order: Vec<String>,
    ) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::ReorderCities {
            task_id: task_id.to_string(),
            new_order,
            reply,
        })
        .await?
    }

    pub async fn reload_tasks(&self) {
        self.send_fire_and_forget(EngineMsg::ReloadTasks).await;
    }

    pub async fn handle_task_reload(self: &Arc<Self>, action: &str, task_id: Option<&str>) {
        self.send_fire_and_forget(EngineMsg::HandleTaskReload {
            action: action.to_string(),
            task_id: task_id.map(|s| s.to_string()),
        })
        .await;
    }

    pub async fn handle_device_kick(self: &Arc<Self>, hw_serials: Vec<String>) -> u32 {
        self.send_and_recv(|reply| EngineMsg::HandleDeviceKick { hw_serials, reply })
            .await
            .unwrap_or(0)
    }

    pub async fn handle_phones_unbind(self: &Arc<Self>, phones: Vec<String>) -> u32 {
        self.send_and_recv(|reply| EngineMsg::HandlePhonesUnbind { phones, reply })
            .await
            .unwrap_or(0)
    }

    pub async fn release_offline_devices(&self, online_serials: &[String]) -> u32 {
        self.send_and_recv(|reply| EngineMsg::ReleaseOfflineDevices {
            online_serials: online_serials.to_vec(),
            reply,
        })
        .await
        .unwrap_or(0)
    }

    /// 推送任务状态到前端（节流控制）— event_loop 内部自动执行
    #[allow(dead_code)]
    pub async fn emit_update(&self) {
        // 在 Message Channel 模型中，emit 由 event_loop 内部控制
        // 外部调用仅作为 hint（event_loop 自动 emit）
    }

    /// 强制推送 — event_loop 内部自动执行
    #[allow(dead_code)]
    pub async fn force_emit_update(&self) {
        // 同上，event_loop 在每个消息处理后自动 emit
    }

    /// 清除 ERROR 任务的设备关联，使关联设备重新可调度
    pub async fn clear_task_device(&self, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::ClearTaskDevice {
            task_id: task_id.to_string(),
            reply,
        })
        .await?
    }

    /// P1 修复：优雅关闭引擎，取消所有 worker 并等待 event_loop 退出
    pub async fn shutdown(&self) {
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = self.tx.send(EngineMsg::Shutdown { reply: reply_tx }).await;
        let _ = reply_rx.await;
    }
}
