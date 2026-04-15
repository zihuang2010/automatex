use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, info};

use crate::constants;
use crate::http::{self, ApiClient, BatchTaskItem, BatchTasksRequest};
use crate::storage::Database;
use crate::task_provider::{CityDef, TaskDef};

pub(crate) fn normalize_unique_strings<I, S>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for value in values {
        let value = value.as_ref().trim();
        if value.is_empty() {
            continue;
        }
        if seen.insert(value.to_string()) {
            normalized.push(value.to_string());
        }
    }
    normalized
}

pub(crate) fn batch_item_to_task_def(item: &BatchTaskItem) -> TaskDef {
    TaskDef {
        id: item.task_id.clone(),
        name: item.task_name.clone(),
        interval_minute: item.interval_minute,
        cities: item
            .city_items
            .iter()
            .map(|city| CityDef {
                name: city.city_name.clone(),
                poi: city.point_name.clone(),
                keywords: city.keywords.clone(),
            })
            .collect(),
    }
}

pub(crate) async fn fetch_batch_task_items(
    http: &Arc<dyn ApiClient>,
    task_ids: &[String],
) -> Result<Vec<BatchTaskItem>, String> {
    let task_ids = normalize_unique_strings(task_ids.iter().map(|id| id.as_str()));
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    let total_chunks = task_ids.len().div_ceil(constants::limits::MAX_BATCH_TASK_IDS_PER_REQUEST);
    for (index, chunk) in
        task_ids.chunks(constants::limits::MAX_BATCH_TASK_IDS_PER_REQUEST).enumerate()
    {
        debug!(
            chunk = index + 1,
            total_chunks = total_chunks,
            task_ids_count = chunk.len(),
            "拉取任务详情分片"
        );
        let chunk_items =
            http.batch_fetch_tasks(&BatchTasksRequest { task_ids: chunk.to_vec() }).await?;
        items.extend(chunk_items);
    }
    Ok(items)
}

pub(crate) async fn merge_batch_task_items(
    db: &Database,
    items: &[BatchTaskItem],
) -> Result<usize, String> {
    let mut upsert_items: Vec<(String, String, String, i64, String)> = Vec::new();
    let mut server_ids = HashSet::new();

    for item in items {
        let def = batch_item_to_task_def(item);
        let payload =
            serde_json::to_string(&def).map_err(|e| format!("序列化任务定义失败: {}", e))?;
        upsert_items.push((def.id.clone(), def.name.clone(), payload, 1, item.mobile.clone()));
        server_ids.insert(def.id);
    }

    // CON-4 修复：用 `?` 传播错误，调用方可感知并中止同步流程
    // 强防御：返回值是实际写入数（拒绝空 phone 孤儿后），不一定等于 items.len()
    let written = db.batch_upsert_task_defs(upsert_items).await?;
    if written < items.len() {
        tracing::error!(
            input = items.len(),
            written,
            skipped = items.len() - written,
            "merge: 部分任务因服务端下发空 mobile 被拒绝"
        );
    }

    let local_defs = db.load_all_task_defs().await;
    let stale_ids: Vec<String> = local_defs
        .iter()
        .filter(|(id, _, _, _)| !server_ids.contains(id))
        .map(|(id, _, _, _)| id.clone())
        .collect();

    if !stale_ids.is_empty() {
        info!(count = stale_ids.len(), stale_ids = ?stale_ids, "清理本地过期任务");
        if let Err(e) = db.batch_cleanup_tasks(&stale_ids).await {
            tracing::error!(error = %e, "批量清理过期任务失败");
        }
    }

    Ok(written)
}

pub(crate) async fn load_remote_tasks_by_ids(
    http: &Arc<dyn ApiClient>,
    db: &Database,
    task_ids: &[String],
) -> Result<usize, String> {
    let items = fetch_batch_task_items(http, task_ids).await?;
    merge_batch_task_items(db, &items).await
}

pub(crate) fn bind_conflict_phones(bind_resp: &http::PhoneBindResponse) -> Vec<String> {
    bind_resp.conflicts.iter().map(|c| c.mobile.clone()).collect()
}
