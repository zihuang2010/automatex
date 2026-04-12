use super::{log_exec, Database};
use rusqlite::params;
use tracing::error;

impl Database {
    pub async fn set_setting(&self, key: &str, value: &str) {
        let key = key.to_string();
        let value = value.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "INSERT OR REPLACE INTO a_settings (key, value) VALUES (?1, ?2)",
                        params![key, value],
                    ),
                    "set_setting",
                );
            })
            .await;
    }

    /// 批量设置（单连接事务内完成，减少池争用）
    pub async fn set_settings_batch(&self, pairs: &[(&str, &str)]) {
        let pairs: Vec<(String, String)> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                let tx = match conn.transaction() {
                    Ok(tx) => tx,
                    Err(e) => {
                        error!(op = "set_settings_batch", error = %e, "事务开始失败");
                        return;
                    },
                };
                for (key, value) in &pairs {
                    log_exec(
                        tx.execute(
                            "INSERT OR REPLACE INTO a_settings (key, value) VALUES (?1, ?2)",
                            params![key, value],
                        ),
                        "set_settings_batch",
                    );
                }
                if let Err(e) = tx.commit() {
                    error!(op = "set_settings_batch", error = %e, "事务提交失败");
                }
            })
            .await;
    }

    /// 批量清理任务（单连接事务内完成，错误向上传播）
    pub async fn batch_cleanup_tasks(&self, task_ids: &[String]) -> Result<(), String> {
        let task_ids = task_ids.to_vec();
        let conn = self.pool.get().await.map_err(|e| format!("获取连接失败: {}", e))?;
        conn.interact(move |conn| {
            let tx = conn.transaction().map_err(|e| format!("事务开始失败: {}", e))?;
            for tid in &task_ids {
                tx.execute("DELETE FROM a_task_progress WHERE task_id = ?1", params![tid])
                    .map_err(|e| format!("删除 progress 失败 ({}): {}", tid, e))?;
                tx.execute("DELETE FROM a_task_results WHERE task_id = ?1", params![tid])
                    .map_err(|e| format!("删除 results 失败 ({}): {}", tid, e))?;
                tx.execute("DELETE FROM a_task_runs WHERE task_id = ?1", params![tid])
                    .map_err(|e| format!("删除 runs 失败 ({}): {}", tid, e))?;
                tx.execute("DELETE FROM a_task_rounds WHERE task_id = ?1", params![tid])
                    .map_err(|e| format!("删除 rounds 失败 ({}): {}", tid, e))?;
                tx.execute("DELETE FROM a_task_state WHERE task_id = ?1", params![tid])
                    .map_err(|e| format!("删除 state 失败 ({}): {}", tid, e))?;
                tx.execute("DELETE FROM a_task_defs WHERE task_id = ?1", params![tid])
                    .map_err(|e| format!("删除 defs 失败 ({}): {}", tid, e))?;
            }
            tx.commit().map_err(|e| format!("事务提交失败: {}", e))?;
            Ok(())
        })
        .await
        .map_err(|e| format!("interact 失败: {}", e))?
    }

    pub async fn get_setting(&self, key: &str) -> Option<String> {
        let key = key.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row("SELECT value FROM a_settings WHERE key = ?1", params![key], |row| {
                row.get(0)
            })
            .ok()
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn get_all_settings(&self) -> std::collections::HashMap<String, String> {
        let Ok(conn) = self.pool.get().await else { return Default::default() };
        conn.interact(|conn| {
            let mut map = std::collections::HashMap::new();
            if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM a_settings") {
                if let Ok(rows) = stmt
                    .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                {
                    for row in rows.flatten() {
                        map.insert(row.0, row.1);
                    }
                }
            }
            map
        })
        .await
        .unwrap_or_default()
    }
}
