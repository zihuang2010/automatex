//! 任务 Worker —— 每个执行中的任务一个独立 tokio task
//!
//! Worker 负责：
//! 1. 每 10s 检查设备在线状态（DB 查询，在 worker 中执行，不阻塞 event loop）
//! 2. 发送 TickRequest 给 event loop（状态变更在 event loop 中串行执行）
//! 3. 收到 TickOutcome 决定继续运行还是退出

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::constants;
use crate::storage::Database;

use super::{EngineMsg, TickOutcome};

/// 启动一个并行 worker，返回 JoinHandle
pub(super) fn spawn_worker(
    task_id: String,
    device_serial: String,
    cancel: CancellationToken,
    tx: mpsc::Sender<EngineMsg>,
    db: Arc<Database>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let exit_success = task_worker_loop(
            &task_id,
            &device_serial,
            &cancel,
            &tx,
            &db,
        )
        .await;

        // 通知 event loop worker 已退出
        let _ = tx
            .send(EngineMsg::WorkerExited {
                task_id,
                success: exit_success,
            })
            .await;
    })
}

/// worker 主循环，返回 true = 任务成功完成，false = 被取消或异常
async fn task_worker_loop(
    task_id: &str,
    device_serial: &str,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<EngineMsg>,
    db: &Database,
) -> bool {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return false,
            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                // Step 1: 在 worker 中查询设备状态（不阻塞 event loop）
                let device_online = db
                    .get_device_by_serial(device_serial)
                    .await
                    .map(|d| d.state == constants::device_state::DEVICE)
                    .unwrap_or(false);

                // Step 2: 发送 tick 请求给 event loop
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                if tx
                    .send(EngineMsg::TickRequest {
                        task_id: task_id.to_string(),
                        device_online,
                        reply: reply_tx,
                    })
                    .await
                    .is_err()
                {
                    return false; // channel closed
                }

                // Step 3: 等待 event loop 的处理结果
                match reply_rx.await {
                    Ok(TickOutcome::Continue) => {} // 继续下一轮
                    Ok(TickOutcome::TaskDone) => return true,
                    Ok(TickOutcome::TaskError) => return false,
                    Err(_) => return false, // event loop 关闭
                }
            }
        }
    }
}
