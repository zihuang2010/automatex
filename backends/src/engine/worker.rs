//! 任务 Worker —— 单次扫描执行单元
//!
//! ## 职责
//! 1. 通过 ADB port-forward 建立 TCP 连接到手机无障碍 App
//! 2. 将本轮所有待扫描的城市 + 关键词作为一个批次发送
//! 3. 流式接收 NDJSON 结果：每收到一条即写入 DB（crash-safe）
//! 4. 批次结束后向 engine event loop 回报结果
//!
//! ## 取消安全
//! `spawn_worker` 在外层 `tokio::select!` 中监听 `CancellationToken`。
//! 取消时正在进行的 TCP 连接 / 读取会被 drop，连接自动关闭。
//! 手机端感知到连接断开后会自行停止当前批次。

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::connection::phone_client::{BatchTask, PhoneClient, PhoneMsg, ScanError};
use crate::storage::Database;

use super::{EngineMsg, ExecutionOutcome};

/// 启动一个异步 Worker task，完成后通过 `tx` 回报结果。
///
/// # 参数
/// - `task_id` — 任务 ID
/// - `device_serial` — 设备 ADB serial（用于日志和 DB 记录）
/// - `round_id` — 当前轮次 ID（用于 DB 进度记录）
/// - `local_port` — PC 本地端口（经 ADB forward 映射到手机 7899）
/// - `pending_tasks` — 本批次待扫描的城市 + 关键词列表
/// - `batch_id` — 批次标识（透传给手机端，用于日志对齐）
/// - `max_pages` — 每个关键词最大翻页数
/// - `worker_seq` — 序列号（用于结果路由，防止过期结果干扰引擎）
/// - `cancel` — 取消令牌（暂停 / 停止时触发）
/// - `tx` — 回报通道
/// - `db` — 数据库句柄（用于实时写入关键词进度）
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_worker(
    task_id: String,
    device_serial: String,
    round_id: i64,
    local_port: u16,
    pending_tasks: Vec<BatchTask>,
    batch_id: String,
    max_pages: u32,
    worker_seq: u64,
    cancel: CancellationToken,
    tx: mpsc::Sender<EngineMsg>,
    db: Arc<Database>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let outcome = tokio::select! {
            biased;
            // 取消优先：CancellationToken 触发时立即中止
            _ = cancel.cancelled() => {
                eprintln!(
                    "[worker] 已取消 (task={}, seq={})",
                    task_id, worker_seq
                );
                ExecutionOutcome::Cancelled
            }
            // 正常执行路径
            result = run_phone_scan(
                &task_id,
                &device_serial,
                round_id,
                local_port,
                &pending_tasks,
                &batch_id,
                max_pages,
                worker_seq,
                &tx,
                &db,
            ) => result,
        };

        // 无论成功 / 失败 / 取消，均回报结果（引擎依赖此消息清理 active_worker）
        let _ = tx.send(EngineMsg::WorkerResult { task_id, worker_seq, outcome }).await;
    })
}

/// 执行一次完整的手机扫描批次。
///
/// 此函数无取消逻辑，由外层 `tokio::select!` + `CancellationToken` 控制。
///
/// # 返回值
/// - `BatchDone { completed, stopped }` — 批次正常结束（含手机端提前停止的情况）
/// - `DeviceOffline` — 连接失败或流意外中断且无已完成结果
async fn run_phone_scan(
    task_id: &str,
    device_serial: &str,
    round_id: i64,
    local_port: u16,
    tasks: &[BatchTask],
    batch_id: &str,
    max_pages: u32,
    worker_seq: u64,
    tx: &mpsc::Sender<EngineMsg>,
    db: &Database,
) -> ExecutionOutcome {
    // ── 快速路径：无待扫描项时直接返回完成 ──
    if tasks.is_empty() {
        eprintln!("[worker] task={} 无待扫描关键词，批次视为已完成", task_id);
        return ExecutionOutcome::BatchDone { completed: Vec::new(), stopped: false };
    }

    let total_keywords: usize = tasks.iter().map(|t| t.keywords.len()).sum();
    let client = PhoneClient::new(local_port);

    // ── 1. 建立连接并发送批次请求 ──
    let mut stream = match client.open(batch_id, tasks, max_pages).await {
        Ok(s) => {
            eprintln!(
                "[worker] 已连接手机 (task={}, device={}, port={}, keywords={})",
                task_id, device_serial, local_port, total_keywords
            );
            s
        },
        Err(ScanError::ConnectionFailed(e)) => {
            eprintln!(
                "[worker] 连接手机失败 (task={}, device={}, port={}): {}",
                task_id, device_serial, local_port, e
            );
            return ExecutionOutcome::DeviceOffline;
        },
        Err(e) => {
            eprintln!("[worker] 建立流失败 (task={}, device={}): {}", task_id, device_serial, e);
            return ExecutionOutcome::DeviceOffline;
        },
    };

    // ── 2. 流式接收 NDJSON 结果 ──
    let mut completed: Vec<(String, String)> = Vec::with_capacity(total_keywords);

    loop {
        match stream.next_msg().await {
            // ── 实时进度（切换城市 / 开始扫描关键词）──
            Ok(Some(PhoneMsg::Progress { city, keyword, status })) => {
                let _ = tx.send(EngineMsg::ScanProgress {
                    task_id: task_id.to_string(),
                    city,
                    keyword,
                    status,
                    worker_seq,
                }).await;
            },

            // ── 单个关键词完成 ──
            Ok(Some(PhoneMsg::Result { city, keyword, items })) => {
                let item_count = items.len() as i32;
                // 实时写入 DB — crash-safe：进程崩溃后下次重启能准确恢复进度
                db.record_keyword_done(task_id, &city, &keyword, device_serial, round_id, item_count).await;
                // 写入采集结果明细
                if !items.is_empty() {
                    let pairs: Vec<(String, String)> = items
                        .iter()
                        .map(|it| (it.name.clone(), it.captured_at.clone()))
                        .collect();
                    db.save_keyword_results(task_id, round_id, &city, &keyword, &pairs).await;
                }
                eprintln!(
                    "[worker] 关键词完成 [{}/{}] task={} city={} keyword={} items={}",
                    completed.len() + 1,
                    total_keywords,
                    task_id,
                    city,
                    keyword,
                    item_count,
                );
                // 通知引擎实时更新内存状态 + 推送前端
                let _ = tx.send(EngineMsg::KeywordDone {
                    task_id: task_id.to_string(),
                    city: city.clone(),
                    keyword: keyword.clone(),
                    worker_seq,
                }).await;
                completed.push((city, keyword));
            },

            // ── 手机端致命错误：立即终止，释放设备 ──
            Ok(Some(PhoneMsg::FatalError { city, reason, .. })) => {
                eprintln!(
                    "[worker] 致命错误 task={} city={} reason={}",
                    task_id, city, reason
                );
                return ExecutionOutcome::FatalError { completed, city, reason };
            },

            // ── 批次全部结束 ──
            Ok(Some(PhoneMsg::Done(info))) => {
                eprintln!(
                    "[worker] 批次结束 task={} total_items={} completed={}/{} stopped={} fatal={:?}",
                    task_id,
                    info.total_items,
                    info.completed_tasks,
                    info.total_tasks,
                    info.stopped,
                    info.fatal_reason,
                );
                return ExecutionOutcome::BatchDone { completed, stopped: info.stopped };
            },

            // ── EOF：连接关闭但未收到 done ──
            Ok(None) => {
                eprintln!(
                    "[worker] 连接提前关闭，未收到 done (task={}, completed={}/{})",
                    task_id,
                    completed.len(),
                    total_keywords,
                );
                // 已有部分结果：作为「提前停止」处理，保留已完成进度
                if !completed.is_empty() {
                    return ExecutionOutcome::BatchDone { completed, stopped: true };
                }
                return ExecutionOutcome::DeviceOffline;
            },

            // ── IO 错误：流中断 ──
            Err(ScanError::StreamBroken(e)) => {
                eprintln!(
                    "[worker] 流中断 (task={}, completed={}/{}): {}",
                    task_id,
                    completed.len(),
                    total_keywords,
                    e
                );
                if !completed.is_empty() {
                    return ExecutionOutcome::BatchDone { completed, stopped: true };
                }
                return ExecutionOutcome::DeviceOffline;
            },

            // ── 其他错误（序列化等内部问题）──
            Err(e) => {
                eprintln!("[worker] 读取错误 (task={}): {}", task_id, e);
                return ExecutionOutcome::DeviceOffline;
            },
        }
    }
}
