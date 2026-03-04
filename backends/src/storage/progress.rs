use super::stats::ProgressRow;
use super::{log_exec, now_unix, today_str, Database};
use rusqlite::params;

impl Database {
    // ─── 轮次操作（a_task_rounds）────────────────────────────────────

    /// 创建新轮次，返回 round_id
    pub async fn create_round(&self, task_id: &str) -> Option<i64> {
        let task_id = task_id.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            let today = today_str();
            let now = now_unix();
            // 事务保证 MAX+INSERT 原子性，避免并发 round_no 重复
            let tx = conn.transaction().ok()?;
            // 先关闭该任务所有旧的 running 轮次，防止僵尸记录
            tx.execute(
                "UPDATE a_task_rounds SET ended_at = ?1, status = 'stopped' WHERE task_id = ?2 AND status = 'running'",
                params![now, task_id],
            ).ok();
            let round_no: i32 = tx
                .query_row(
                    "SELECT COALESCE(MAX(round_no), 0) + 1 FROM a_task_rounds WHERE task_id = ?1 AND run_date = ?2",
                    params![task_id, today],
                    |row| row.get(0),
                )
                .unwrap_or(1);
            tx.execute(
                "INSERT INTO a_task_rounds (task_id, run_date, round_no, started_at, status)
                 VALUES (?1, ?2, ?3, ?4, 'running')",
                params![task_id, today, round_no, now],
            ).ok()?;
            let id = tx.last_insert_rowid();
            tx.commit().ok()?;
            eprintln!("[db] 创建轮次: task={}, date={}, round_no={}, id={}", task_id, today, round_no, id);
            Some(id)
        })
        .await
        .ok()
        .flatten()
    }

    /// 结束轮次
    pub async fn finish_round(&self, round_id: i64, status: &str) {
        let status = status.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                let now = now_unix();
                log_exec(
                    conn.execute(
                        "UPDATE a_task_rounds SET ended_at = ?1, status = ?2 WHERE id = ?3",
                        params![now, status, round_id],
                    ),
                    "finish_round",
                );
            })
            .await;
    }

    /// 获取当前运行中的轮次 ID
    #[allow(dead_code)]
    pub async fn get_active_round(&self, task_id: &str) -> Option<i64> {
        let task_id = task_id.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT id FROM a_task_rounds WHERE task_id = ?1 AND status = 'running' ORDER BY id DESC LIMIT 1",
                params![task_id],
                |row| row.get(0),
            )
            .ok()
        })
        .await
        .ok()
        .flatten()
    }

    /// 批量加载轮次信息（round_id → round_no），用于 load_tasks 消除 N+1
    pub async fn load_all_round_info(
        &self,
        round_ids: &[i64],
    ) -> std::collections::HashMap<i64, i32> {
        if round_ids.is_empty() {
            return Default::default();
        }
        let round_ids = round_ids.to_vec();
        let Ok(conn) = self.pool.get().await else {
            return Default::default();
        };
        conn.interact(move |conn| {
            let mut map = std::collections::HashMap::new();
            for chunk in round_ids.chunks(500) {
                let placeholders: Vec<String> =
                    (0..chunk.len()).map(|i| format!("?{}", i + 1)).collect();
                let sql = format!(
                    "SELECT id, round_no FROM a_task_rounds WHERE id IN ({})",
                    placeholders.join(",")
                );
                if let Ok(mut stmt) = conn.prepare(&sql) {
                    let param_values: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
                        .iter()
                        .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
                        .collect();
                    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                        param_values.iter().map(|p| p.as_ref()).collect();
                    if let Ok(rows) = stmt.query_map(params_ref.as_slice(), |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, i32>(1)?))
                    }) {
                        for row in rows.flatten() {
                            map.insert(row.0, row.1);
                        }
                    }
                }
            }
            map
        })
        .await
        .unwrap_or_default()
    }

    /// 加载单个任务的轮次号（单任务构建用）
    pub async fn get_round_no(&self, round_id: i64) -> Option<i32> {
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT round_no FROM a_task_rounds WHERE id = ?1",
                params![round_id],
                |row| row.get(0),
            )
            .ok()
        })
        .await
        .ok()
        .flatten()
    }

    // ─── 执行进度操作 ────────────────────────────────────────────

    pub async fn record_keyword_done(
        &self,
        task_id: &str,
        city_name: &str,
        keyword_name: &str,
        device_serial: &str,
        round_id: i64,
    ) {
        let task_id = task_id.to_string();
        let city_name = city_name.to_string();
        let keyword_name = keyword_name.to_string();
        let device_serial = device_serial.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(move |conn| {
            let now = now_unix();
            log_exec(
                conn.execute(
                    "INSERT OR IGNORE INTO a_task_progress
                        (task_id, city_name, keyword_name, round_id, status, completed_at, device_serial, sync_status)
                     VALUES (?1, ?2, ?3, ?4, 'ok', ?5, ?6, 'pending')",
                    params![task_id, city_name, keyword_name, round_id, now, device_serial],
                ),
                "record_keyword_done",
            );
        }).await;
    }

    /// 加载指定轮次的任务进度
    pub async fn load_task_progress(&self, task_id: &str, round_id: i64) -> Vec<ProgressRow> {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT task_id, city_name, keyword_name, status, completed_at, device_serial
                 FROM a_task_progress WHERE task_id = ?1 AND round_id = ?2",
            )?;
            let rows = stmt.query_map(params![task_id, round_id], |row| {
                Ok(ProgressRow {
                    task_id: row.get(0)?,
                    city_name: row.get(1)?,
                    keyword_name: row.get(2)?,
                    status: row.get(3)?,
                    completed_at: row.get(4)?,
                    device_serial: row.get(5)?,
                })
            })?;
            Ok::<_, rusqlite::Error>(rows.filter_map(|r| r.ok()).collect())
        })
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default()
    }

    /// 批量加载指定轮次的全部进度（用于 load_tasks 消除 N+1）
    pub async fn load_all_progress(&self, round_ids: &[i64]) -> Vec<ProgressRow> {
        if round_ids.is_empty() {
            return Vec::new();
        }
        let round_ids = round_ids.to_vec();
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(move |conn| {
            let mut all_rows = Vec::new();
            for chunk in round_ids.chunks(500) {
                let placeholders: Vec<String> =
                    (0..chunk.len()).map(|i| format!("?{}", i + 1)).collect();
                let sql = format!(
                    "SELECT task_id, city_name, keyword_name, status, completed_at, device_serial
                     FROM a_task_progress WHERE round_id IN ({})",
                    placeholders.join(",")
                );
                let mut stmt = conn.prepare(&sql)?;
                let param_values: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
                    .iter()
                    .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
                    .collect();
                let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                    param_values.iter().map(|p| p.as_ref()).collect();
                let rows = stmt.query_map(params_ref.as_slice(), |row| {
                    Ok(ProgressRow {
                        task_id: row.get(0)?,
                        city_name: row.get(1)?,
                        keyword_name: row.get(2)?,
                        status: row.get(3)?,
                        completed_at: row.get(4)?,
                        device_serial: row.get(5)?,
                    })
                })?;
                all_rows.extend(rows.filter_map(|r| r.ok()));
            }
            Ok::<_, rusqlite::Error>(all_rows)
        })
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default()
    }

    pub async fn clear_task_progress(&self, task_id: &str) {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "DELETE FROM a_task_progress WHERE task_id = ?1",
                        params![task_id],
                    ),
                    "clear_task_progress",
                );
            })
            .await;
    }

    pub async fn cleanup_orphan_progress(&self, task_id: &str, valid_pairs: Vec<(String, String)>) {
        if valid_pairs.is_empty() {
            self.clear_task_progress(task_id).await;
            return;
        }
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                // 使用 (city_name, keyword_name) 双列判断，避免分隔符拼接导致误匹配
                let pair_conditions: Vec<String> = (0..valid_pairs.len())
                    .map(|i| {
                        format!("(city_name = ?{} AND keyword_name = ?{})", i * 2 + 2, i * 2 + 3)
                    })
                    .collect();
                let sql = format!(
                    "DELETE FROM a_task_progress WHERE task_id = ?1 AND NOT ({})",
                    pair_conditions.join(" OR ")
                );
                let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
                    Vec::with_capacity(valid_pairs.len() * 2 + 1);
                param_values.push(Box::new(task_id.clone()));
                for (city, keyword) in &valid_pairs {
                    param_values.push(Box::new(city.clone()));
                    param_values.push(Box::new(keyword.clone()));
                }
                let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                    param_values.iter().map(|p| p.as_ref()).collect();
                let result = conn.execute(&sql, params_ref.as_slice());
                match result {
                    Ok(n) if n > 0 => {
                        eprintln!("[db] 清理了 {} 条孤儿进度记录 (task={})", n, task_id)
                    },
                    Err(e) => eprintln!("[db] cleanup_orphan_progress 失败: {}", e),
                    _ => {},
                }
            })
            .await;
    }

    #[allow(dead_code)]
    pub async fn load_pending_progress(&self) -> Vec<ProgressRow> {
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(|conn| {
            let mut stmt = conn.prepare(
                "SELECT task_id, city_name, keyword_name, status, completed_at, device_serial
                 FROM a_task_progress WHERE sync_status = 'pending'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(ProgressRow {
                    task_id: row.get(0)?,
                    city_name: row.get(1)?,
                    keyword_name: row.get(2)?,
                    status: row.get(3)?,
                    completed_at: row.get(4)?,
                    device_serial: row.get(5)?,
                })
            })?;
            Ok::<_, rusqlite::Error>(rows.filter_map(|r| r.ok()).collect())
        })
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub async fn mark_progress_synced(&self, task_id: &str, city_name: &str, keyword_name: &str) {
        let task_id = task_id.to_string();
        let city_name = city_name.to_string();
        let keyword_name = keyword_name.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "UPDATE a_task_progress SET sync_status = 'synced'
                     WHERE task_id = ?1 AND city_name = ?2 AND keyword_name = ?3",
                        params![task_id, city_name, keyword_name],
                    ),
                    "mark_progress_synced",
                );
            })
            .await;
    }
}
