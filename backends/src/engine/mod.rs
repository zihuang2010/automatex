mod event_loop;
mod worker;

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use crate::http;
use crate::storage::Database;
use crate::task_provider::{self, Task};

// ─── 公共类型 ──────────────────────────────────────────

/// 零拷贝快照：序列化时直接引用 tasks
#[derive(Serialize)]
pub(crate) struct TaskSnapshotRef<'a> {
    pub tasks: &'a [Task],
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
        reply: oneshot::Sender<Vec<Task>>,
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
    TickRequest {
        task_id: String,
        device_online: bool,
        reply: oneshot::Sender<TickOutcome>,
    },
    WorkerExited {
        task_id: String,
        success: bool,
    },
}

/// tick 处理结果，告知 worker 下一步
#[derive(Debug)]
pub(crate) enum TickOutcome {
    Continue,
    TaskDone,
    TaskError,
}

// ─── TaskEngine（thin sender wrapper）─────────────────

const ENGINE_CHANNEL_SIZE: usize = 256;

pub struct TaskEngine {
    tx: mpsc::Sender<EngineMsg>,
}

impl TaskEngine {
    /// 初始化引擎：加载任务 → 启动事件循环 → 返回 handle
    pub async fn new(
        storage: Arc<Database>,
        http: Arc<dyn http::ApiClient>,
        app_handle: tauri::AppHandle,
    ) -> Arc<Self> {
        let tasks = task_provider::load_tasks(&storage).await;
        let (tx, rx) = mpsc::channel(ENGINE_CHANNEL_SIZE);

        // 启动事件循环
        event_loop::spawn(rx, tx.clone(), tasks, storage, http, app_handle);

        Arc::new(Self { tx })
    }

    // ── 辅助：发送消息并等待回复 ──

    async fn send_and_recv<T>(
        &self,
        msg_fn: impl FnOnce(oneshot::Sender<T>) -> EngineMsg,
    ) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(msg_fn(reply_tx))
            .await
            .map_err(|_| "引擎已关闭".to_string())?;
        reply_rx.await.map_err(|_| "引擎响应丢失".to_string())
    }

    async fn send_fire_and_forget(&self, msg: EngineMsg) {
        let _ = self.tx.send(msg).await;
    }

    // ── 公共 API（与旧接口完全兼容）──

    pub async fn get_tasks(&self) -> Vec<Task> {
        self.send_and_recv(|reply| EngineMsg::GetTasks { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn start_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::StartTask {
            task_id: task_id.to_string(),
            reply,
        })
        .await?
    }

    pub async fn pause_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::PauseTask {
            task_id: task_id.to_string(),
            reply,
        })
        .await?
    }

    pub async fn resume_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::ResumeTask {
            task_id: task_id.to_string(),
            reply,
        })
        .await?
    }

    pub async fn stop_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::StopTask {
            task_id: task_id.to_string(),
            reply,
        })
        .await?
    }

    pub async fn retry_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        self.send_and_recv(|reply| EngineMsg::RetryTask {
            task_id: task_id.to_string(),
            reply,
        })
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
}
