//! 后台进度上报协程（Outbox Flusher）
//!
//! 定时扫描 `a_task_progress` 中 `sync_status = 'pending'` 的记录，
//! 逐条构造 `ProgressReportRequest` 并调用 HTTP 上报接口。
//! 上报成功后标记为 `'synced'`，失败则保留 pending 等待下轮重试。
//!
//! 与引擎快速路径互为补充：
//! - 快速路径：KeywordDone 时即时 fire-and-forget 上报
//! - 本模块：兜底覆盖快速路径失败、进程重启、远端服务恢复等场景

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::http;
use crate::http::types::ProgressReportRequest;
use crate::storage::Database;

/// 有 pending 记录时的扫描间隔
const FLUSH_INTERVAL_BUSY: Duration = Duration::from_secs(30);
/// 无 pending 记录时的扫描间隔
const FLUSH_INTERVAL_IDLE: Duration = Duration::from_secs(120);

pub fn spawn(
    storage: Arc<Database>,
    http: Arc<dyn http::ApiClient>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        eprintln!("[flusher] 后台进度上报协程已启动");

        loop {
            // 响应取消信号
            if cancel.is_cancelled() {
                break;
            }

            let pending = storage.load_pending_progress().await;
            let has_pending = !pending.is_empty();

            if has_pending {
                eprintln!("[flusher] 发现 {} 条 pending 上报记录", pending.len());
            }

            for row in &pending {
                if cancel.is_cancelled() {
                    break;
                }

                // 查询补充字段
                let Some(ctx) = storage
                    .load_upload_context(
                        &row.task_id,
                        row.round_id,
                        &row.city_name,
                        &row.keyword_name,
                    )
                    .await
                else {
                    eprintln!(
                        "[flusher] 跳过: 无法加载上下文 task={} city={} kw={}",
                        row.task_id, row.city_name, row.keyword_name
                    );
                    continue;
                };

                let req = ProgressReportRequest {
                    client_id: ctx.client_id,
                    task_id: row.task_id.clone(),
                    task_name: ctx.task_name,
                    city_name: row.city_name.clone(),
                    keyword: row.keyword_name.clone(),
                    device_no: row.device_serial.clone(),
                    round_no: ctx.round_no,
                    store_list: ctx.store_list,
                    scan_finished_time: crate::utils::format_datetime(row.completed_at),
                };

                match http.report_progress(&req).await {
                    Ok(_) => {
                        storage
                            .mark_progress_synced(
                                &row.task_id,
                                &row.city_name,
                                &row.keyword_name,
                                row.round_id,
                            )
                            .await;
                        eprintln!(
                            "[flusher] 上报成功: task={} city={} kw={}",
                            row.task_id, row.city_name, row.keyword_name
                        );
                    },
                    Err(e) => {
                        eprintln!(
                            "[flusher] 上报失败 (下轮重试): task={} city={} kw={} err={}",
                            row.task_id, row.city_name, row.keyword_name, e
                        );
                        // 单条失败后跳出本轮，避免对不可用服务端连续请求
                        break;
                    },
                }
            }

            let interval = if has_pending { FLUSH_INTERVAL_BUSY } else { FLUSH_INTERVAL_IDLE };

            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(interval) => {},
            }
        }

        eprintln!("[flusher] 后台进度上报协程已退出");
    })
}
