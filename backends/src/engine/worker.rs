//! 任务 Worker —— 单次执行单元
//!
//! Worker 负责：
//! 1. 执行一次任务动作（当前实现仍是抽象 tick）
//! 2. 查询本次执行所需的外部状态
//! 3. 回报明确结果给 event loop

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::constants;
use crate::storage::Database;

use super::{EngineMsg, ExecutionOutcome};

pub(super) fn spawn_worker(
    task_id: String,
    device_serial: String,
    worker_seq: u64,
    cancel: CancellationToken,
    tx: mpsc::Sender<EngineMsg>,
    db: Arc<Database>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let outcome = task_worker_once(&device_serial, &cancel, &db)
            .await
            .unwrap_or(ExecutionOutcome::Cancelled);

        let _ = tx.send(EngineMsg::WorkerResult { task_id, worker_seq, outcome }).await;
    })
}

async fn task_worker_once(
    device_serial: &str,
    cancel: &CancellationToken,
    db: &Database,
) -> Option<ExecutionOutcome> {
    tokio::select! {
        _ = cancel.cancelled() => None,
        result = db.get_device_by_serial(device_serial) => {
            let online = result
                .map(|d| d.state == constants::device_state::DEVICE)
                .unwrap_or(false);

            if online {
                Some(ExecutionOutcome::Success {
                    next_delay_ms: constants::timing::TASK_DISPATCH_INTERVAL_SECS * 1000,
                })
            } else {
                Some(ExecutionOutcome::DeviceOffline)
            }
        }
    }
}
