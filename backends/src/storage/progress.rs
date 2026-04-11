use super::stats::ProgressRow;
use super::{log_exec, now_unix, today_str, Database};
use rusqlite::params;
use serde::Serialize;

/// 关键词采集结果条目（从 a_task_results 查询）
#[derive(Debug, Clone, Serialize)]
pub struct ResultRow {
    pub shop_name: String,
    pub captured_at: String,
    pub round_id: i64,
}

/// pending 上报记录的补充上下文（从 DB 查询拼装）
#[derive(Debug)]
pub struct UploadContext {
    pub task_name: String,
    pub round_no: i32,
    pub store_list: Vec<String>,
    pub client_id: String,
}

impl Database {
    // ─── 轮次操作（a_task_rounds）────────────────────────────────────

    /// 创建新轮次，返回 round_id
    pub async fn create_round(&self, task_id: &str) -> Option<i64> {
        let task_id = task_id.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            let today = today_str();
            let now = now_unix();
            // Fix-SQL4：使用 IMMEDIATE 事务，在事务开始时即获取写锁，
            // 保证 SELECT MAX + INSERT 的原子性（避免并发连接读到相同 MAX 值）
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).ok()?;
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

    pub async fn resume_round(&self, round_id: i64) -> bool {
        let Ok(conn) = self.pool.get().await else { return false };
        conn.interact(move |conn| {
            conn.execute(
                "UPDATE a_task_rounds SET ended_at = NULL, status = 'running' WHERE id = ?1",
                params![round_id],
            )
            .map(|n| n > 0)
            .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
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
            // Fix-SQL2：使用 params_from_iter 替代 Box<dyn ToSql> 逐个堆分配
            for chunk in round_ids.chunks(500) {
                let placeholders: Vec<String> =
                    (0..chunk.len()).map(|i| format!("?{}", i + 1)).collect();
                let sql = format!(
                    "SELECT id, round_no FROM a_task_rounds WHERE id IN ({})",
                    placeholders.join(",")
                );
                if let Ok(mut stmt) = conn.prepare(&sql) {
                    if let Ok(rows) = stmt
                        .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                            Ok((row.get::<_, i64>(0)?, row.get::<_, i32>(1)?))
                        })
                    {
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
        item_count: i32,
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
                    "INSERT INTO a_task_progress
                        (task_id, city_name, keyword_name, round_id, status, completed_at, device_serial, sync_status, item_count)
                     VALUES (?1, ?2, ?3, ?4, 'ok', ?5, ?6, 'pending', ?7)
                     ON CONFLICT(task_id, city_name, keyword_name, round_id) DO UPDATE SET
                         item_count = excluded.item_count",
                    params![task_id, city_name, keyword_name, round_id, now, device_serial, item_count],
                ),
                "record_keyword_done",
            );
        }).await;
    }

    /// 批量写入关键词采集结果（单事务）
    pub async fn save_keyword_results(
        &self,
        task_id: &str,
        round_id: i64,
        city_name: &str,
        keyword_name: &str,
        items: &[(String, String)], // (shop_name, captured_at)
    ) {
        if items.is_empty() {
            return;
        }
        let task_id = task_id.to_string();
        let city_name = city_name.to_string();
        let keyword_name = keyword_name.to_string();
        let items = items.to_vec();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(move |conn| {
            let now = now_unix();
            // Fix-L2：将 DELETE 移入事务内，保证 DELETE + INSERT 原子性，
            // 避免事务失败时已删除的旧数据无法恢复
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM a_task_results WHERE task_id=?1 AND round_id=?2 AND city_name=?3 AND keyword_name=?4",
                params![task_id, round_id, city_name, keyword_name],
            )?;
            // Fix-SQL1：使用 prepared statement 预编译，避免每条 INSERT 重复 parse SQL
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO a_task_results (task_id, round_id, city_name, keyword_name, shop_name, captured_at, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?;
                for (shop_name, captured_at) in &items {
                    stmt.execute(params![task_id, round_id, city_name, keyword_name, shop_name, captured_at, now])?;
                }
            }
            tx.commit()?;
            Ok::<_, rusqlite::Error>(())
        }).await;
    }

    /// 查询关键词采集结果（仅当天的所有轮次，按轮次倒序）
    pub async fn get_keyword_results(
        &self,
        task_id: &str,
        city_name: &str,
        keyword_name: &str,
    ) -> Vec<crate::storage::ResultRow> {
        let task_id = task_id.to_string();
        let city_name = city_name.to_string();
        let keyword_name = keyword_name.to_string();
        let today = crate::storage::today_str();
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT r.shop_name, r.captured_at, r.round_id
                 FROM a_task_results r
                 JOIN a_task_rounds rnd ON r.round_id = rnd.id
                 WHERE r.task_id = ?1 AND r.city_name = ?2 AND r.keyword_name = ?3
                   AND rnd.run_date = ?4
                 ORDER BY r.round_id DESC, r.id ASC
                 LIMIT 500",
            )?;
            let rows = stmt.query_map(params![task_id, city_name, keyword_name, today], |row| {
                Ok(crate::storage::ResultRow {
                    shop_name: row.get(0)?,
                    captured_at: row.get(1)?,
                    round_id: row.get(2)?,
                })
            })?;
            Ok::<_, rusqlite::Error>(rows.filter_map(|r| r.ok()).collect())
        })
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default()
    }

    /// 加载指定轮次的任务进度
    pub async fn load_task_progress(&self, task_id: &str, round_id: i64) -> Vec<ProgressRow> {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT task_id, city_name, keyword_name, status, completed_at, device_serial, round_id
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
                    round_id: row.get(6)?,
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
            // Fix-SQL2：使用 params_from_iter 替代 Box<dyn ToSql> 逐个堆分配
            for chunk in round_ids.chunks(500) {
                let placeholders: Vec<String> =
                    (0..chunk.len()).map(|i| format!("?{}", i + 1)).collect();
                let sql = format!(
                    "SELECT task_id, city_name, keyword_name, status, completed_at, device_serial, round_id
                     FROM a_task_progress WHERE round_id IN ({})",
                    placeholders.join(",")
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                    Ok(ProgressRow {
                        task_id: row.get(0)?,
                        city_name: row.get(1)?,
                        keyword_name: row.get(2)?,
                        status: row.get(3)?,
                        completed_at: row.get(4)?,
                        device_serial: row.get(5)?,
                        round_id: row.get(6)?,
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

    /// 清除已同步的进度记录，保留未上报的 pending 记录（防止丢失未上报数据）
    pub async fn clear_task_progress(&self, task_id: &str) {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "DELETE FROM a_task_progress WHERE task_id = ?1 AND sync_status = 'synced'",
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
                // 分批处理：每批最多 400 对（800 参数 + 1 task_id = 801 < SQLite 999 上限）
                const BATCH_SIZE: usize = 400;

                // 用临时表方式：先插入有效对，再删除不在其中的
                conn.execute_batch(
                    "CREATE TEMP TABLE IF NOT EXISTS _valid_pairs (city TEXT, keyword TEXT)",
                )
                .ok();
                conn.execute("DELETE FROM _valid_pairs", []).ok();

                for chunk in valid_pairs.chunks(BATCH_SIZE) {
                    let placeholders: Vec<String> = (0..chunk.len())
                        .map(|i| format!("(?{}, ?{})", i * 2 + 1, i * 2 + 2))
                        .collect();
                    let sql = format!(
                        "INSERT INTO _valid_pairs (city, keyword) VALUES {}",
                        placeholders.join(", ")
                    );
                    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
                        Vec::with_capacity(chunk.len() * 2);
                    for (city, keyword) in chunk {
                        param_values.push(Box::new(city.clone()));
                        param_values.push(Box::new(keyword.clone()));
                    }
                    let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                        param_values.iter().map(|p| p.as_ref()).collect();
                    conn.execute(&sql, params_ref.as_slice()).ok();
                }

                let result = conn.execute(
                    "DELETE FROM a_task_progress WHERE task_id = ?1 AND NOT EXISTS (
                        SELECT 1 FROM _valid_pairs WHERE city = city_name AND keyword = keyword_name
                    )",
                    params![task_id],
                );
                match result {
                    Ok(n) if n > 0 => {
                        eprintln!("[db] 清理了 {} 条孤儿进度记录 (task={})", n, task_id);
                    },
                    Err(e) => eprintln!("[db] cleanup_orphan_progress 失败: {}", e),
                    _ => {},
                }

                conn.execute("DROP TABLE IF EXISTS _valid_pairs", []).ok();
            })
            .await;
    }

    pub async fn load_pending_progress(&self) -> Vec<ProgressRow> {
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(|conn| {
            let mut stmt = conn.prepare(
                "SELECT task_id, city_name, keyword_name, status, completed_at, device_serial, round_id
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
                    round_id: row.get(6)?,
                })
            })?;
            Ok::<_, rusqlite::Error>(rows.filter_map(|r| r.ok()).collect())
        })
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default()
    }

    /// 清理超过 N 天的采集结果（通过 a_task_rounds.run_date 判断）
    pub async fn cleanup_old_results(&self, keep_days: u32) {
        let cutoff = {
            let today = chrono::Local::now();
            let d = today - chrono::Duration::days(keep_days as i64);
            d.format("%Y-%m-%d").to_string()
        };
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                let result = conn.execute(
                    "DELETE FROM a_task_results
                     WHERE round_id IN (
                         SELECT id FROM a_task_rounds WHERE run_date < ?1
                     )",
                    rusqlite::params![cutoff],
                );
                match result {
                    Ok(n) if n > 0 => {
                        eprintln!("[db] 清理过期采集结果: {} 条（保留 {} 天）", n, keep_days)
                    },
                    Err(e) => eprintln!("[db] cleanup_old_results 失败: {}", e),
                    _ => {},
                }
            })
            .await;
    }

    pub async fn mark_progress_synced(
        &self,
        task_id: &str,
        city_name: &str,
        keyword_name: &str,
        round_id: i64,
    ) {
        let task_id = task_id.to_string();
        let city_name = city_name.to_string();
        let keyword_name = keyword_name.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "UPDATE a_task_progress SET sync_status = 'synced'
                         WHERE task_id = ?1 AND city_name = ?2 AND keyword_name = ?3 AND round_id = ?4",
                        params![task_id, city_name, keyword_name, round_id],
                    ),
                    "mark_progress_synced",
                );
            })
            .await;
    }

    /// 为 pending 上报记录查询补充信息（task_name, round_no, store_list）
    pub async fn load_upload_context(
        &self,
        task_id: &str,
        round_id: i64,
        city_name: &str,
        keyword_name: &str,
    ) -> Option<UploadContext> {
        let task_id = task_id.to_string();
        let city_name = city_name.to_string();
        let keyword_name = keyword_name.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            // 查 task_name + round_no
            let (task_name, round_no): (String, i32) = conn
                .query_row(
                    "SELECT d.name, r.round_no
                     FROM a_task_defs d
                     JOIN a_task_rounds r ON r.id = ?2
                     WHERE d.task_id = ?1",
                    params![task_id, round_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .ok()?;

            // 查 store_list
            let mut stmt = conn
                .prepare(
                    "SELECT shop_name FROM a_task_results
                     WHERE task_id = ?1 AND round_id = ?2 AND city_name = ?3 AND keyword_name = ?4",
                )
                .ok()?;
            let store_list: Vec<String> = stmt
                .query_map(params![task_id, round_id, city_name, keyword_name], |row| row.get(0))
                .ok()?
                .filter_map(|r| r.ok())
                .collect();

            // 查 client_id
            let client_id: String = conn
                .query_row("SELECT value FROM a_settings WHERE key = 'mqtt_client_id'", [], |row| {
                    row.get(0)
                })
                .unwrap_or_default();

            Some(UploadContext { task_name, round_no, store_list, client_id })
        })
        .await
        .ok()?
    }
}
