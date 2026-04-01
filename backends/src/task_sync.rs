use std::collections::HashSet;
use std::sync::Arc;

use crate::http::{self, ApiClient, BatchTaskItem, BatchTasksRequest};
use crate::storage::Database;
use crate::task_provider::{CityDef, TaskDef};

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
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }
    http.batch_fetch_tasks(&BatchTasksRequest { task_ids: task_ids.to_vec() }).await
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

    db.batch_upsert_task_defs(upsert_items).await;

    let local_defs = db.load_all_task_defs().await;
    let stale_ids: Vec<String> = local_defs
        .iter()
        .filter(|(id, _, _, _)| !server_ids.contains(id))
        .map(|(id, _, _, _)| id.clone())
        .collect();

    if !stale_ids.is_empty() {
        eprintln!("[task_sync] 清理 {} 个本地过期任务: {:?}", stale_ids.len(), stale_ids);
        db.batch_cleanup_tasks(&stale_ids).await;
    }

    Ok(items.len())
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
    bind_resp.conflicts.iter().map(|c| c.phone.clone()).collect()
}
