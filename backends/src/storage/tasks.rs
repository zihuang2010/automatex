use super::{log_exec, now_unix, Database};
use rusqlite::params;

impl Database {
    // ─── 任务定义操作（a_task_defs）─────────────────────────────────

    pub async fn load_all_task_defs(&self) -> Vec<(String, String, String, Option<String>)> {
        let conn = match self.pool.get().await {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        conn.interact(|conn| {
            let mut stmt =
                conn.prepare("SELECT task_id, name, payload, city_order FROM a_task_defs")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;
            Ok::<_, rusqlite::Error>(rows.filter_map(|r| r.ok()).collect())
        })
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default()
    }

    pub async fn load_task_def_by_id(&self, task_id: &str) -> Option<(String, String, String)> {
        let task_id = task_id.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT task_id, name, payload FROM a_task_defs WHERE task_id = ?1",
                params![task_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .ok()
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn upsert_task_def(
        &self,
        task_id: &str,
        name: &str,
        payload: &str,
        version: i64,
        phone: &str,
    ) {
        let task_id = task_id.to_string();
        let name = name.to_string();
        let payload = payload.to_string();
        let phone = phone.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                let now = now_unix();
                log_exec(
                conn.execute(
                    "INSERT INTO a_task_defs (task_id, name, payload, version, fetched_at, phone)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(task_id) DO UPDATE SET
                         name=excluded.name, payload=excluded.payload,
                         version=excluded.version, fetched_at=excluded.fetched_at,
                         phone=CASE WHEN excluded.phone = '' THEN a_task_defs.phone ELSE excluded.phone END",
                    params![task_id, name, payload, version, now, phone],
                ),
                "upsert_task_def",
            );
            })
            .await;
    }

    /// 批量 upsert 任务定义（单事务，减少连接开销）
    /// items: Vec<(task_id, name, payload, version, phone)>
    pub async fn batch_upsert_task_defs(&self, items: Vec<(String, String, String, i64, String)>) {
        if items.is_empty() {
            return;
        }
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                let now = now_unix();
                let tx = match conn.transaction() {
                    Ok(tx) => tx,
                    Err(e) => {
                        eprintln!("[db] batch_upsert_task_defs 事务失败: {}", e);
                        return;
                    }
                };
                for (task_id, name, payload, version, phone) in &items {
                    let _ = tx.execute(
                        "INSERT INTO a_task_defs (task_id, name, payload, version, fetched_at, phone)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(task_id) DO UPDATE SET
                             name=excluded.name, payload=excluded.payload,
                             version=excluded.version, fetched_at=excluded.fetched_at,
                             phone=CASE WHEN excluded.phone = '' THEN a_task_defs.phone ELSE excluded.phone END",
                        params![task_id, name, payload, version, now, phone],
                    );
                }
                if let Err(e) = tx.commit() {
                    eprintln!("[db] batch_upsert_task_defs 提交失败: {}", e);
                } else {
                    eprintln!("[db] batch_upsert: {} 条任务定义", items.len());
                }
            })
            .await;
    }

    pub async fn get_tasks_by_phone(&self, phone: &str) -> Vec<String> {
        let phone = phone.to_string();
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(move |conn| {
            let mut stmt = match conn.prepare("SELECT task_id FROM a_task_defs WHERE phone = ?1") {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[db] get_tasks_by_phone prepare 失败: {}", e);
                    return Vec::new();
                },
            };
            stmt.query_map(params![phone], |row| row.get::<_, String>(0))
                .ok()
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    pub async fn save_city_order(&self, task_id: &str, order: &[String]) {
        let task_id = task_id.to_string();
        let json = serde_json::to_string(order).unwrap_or_default();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "UPDATE a_task_defs SET city_order = ?2 WHERE task_id = ?1",
                        params![task_id, json],
                    ),
                    "save_city_order",
                );
            })
            .await;
    }

    pub async fn load_city_order(&self, task_id: &str) -> Option<Vec<String>> {
        let task_id = task_id.to_string();
        let conn = self.pool.get().await.ok()?;
        let json: Option<String> = conn
            .interact(move |conn| {
                conn.query_row(
                    "SELECT city_order FROM a_task_defs WHERE task_id = ?1",
                    params![task_id],
                    |row| row.get(0),
                )
                .ok()
            })
            .await
            .ok()
            .flatten()?;
        json.and_then(|s| serde_json::from_str(&s).ok())
    }

    // ─── 任务运行时状态操作（a_task_state）────────────────────────────

    pub async fn save_task_state(
        &self,
        task_id: &str,
        status: &str,
        assigned_device: Option<&str>,
        current_round_id: Option<i64>,
    ) {
        let task_id = task_id.to_string();
        let status = status.to_string();
        let assigned_device = assigned_device.map(|s| s.to_string());
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "INSERT INTO a_task_state (task_id, status, assigned_device, current_round_id)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(task_id) DO UPDATE SET
                             status=excluded.status,
                             assigned_device=excluded.assigned_device,
                             current_round_id=excluded.current_round_id",
                        params![task_id, status, assigned_device, current_round_id],
                    ),
                    "save_task_state",
                );
            })
            .await;
    }

    pub async fn load_task_state(
        &self,
        task_id: &str,
    ) -> Option<(String, Option<String>, Option<i64>)> {
        let task_id = task_id.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT status, assigned_device, current_round_id FROM a_task_state WHERE task_id = ?1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok()
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn delete_task_state(&self, task_id: &str) {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute("DELETE FROM a_task_state WHERE task_id = ?1", params![task_id]),
                    "delete_task_state",
                );
            })
            .await;
    }

    /// 批量加载全部任务状态（用于 load_tasks 消除 N+1）
    pub async fn load_all_task_states(
        &self,
    ) -> std::collections::HashMap<String, (String, Option<String>, Option<i64>)> {
        let Ok(conn) = self.pool.get().await else {
            return Default::default();
        };
        conn.interact(|conn| {
            let mut map = std::collections::HashMap::new();
            if let Ok(mut stmt) = conn.prepare(
                "SELECT task_id, status, assigned_device, current_round_id FROM a_task_state",
            ) {
                if let Ok(rows) = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                }) {
                    for row in rows.flatten() {
                        map.insert(row.0, (row.1, row.2, row.3));
                    }
                }
            }
            map
        })
        .await
        .unwrap_or_default()
    }

    /// 跨日重置：所有任务状态回到 WAITING
    pub async fn daily_reset_tasks(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(|conn| {
                let result = conn.execute(
                    "UPDATE a_task_state SET status = 'waiting', assigned_device = NULL, current_round_id = NULL",
                    [],
                );
                match result {
                    Ok(n) => eprintln!("[db] 跨日重置: 重置了 {} 个任务状态", n),
                    Err(e) => eprintln!("[db] daily_reset_tasks 失败: {}", e),
                }
            })
            .await;
    }

    /// 跨日重置：关闭所有未结束的 running 轮次
    pub async fn close_all_running_rounds(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(|conn| {
                let now = now_unix();
                let result = conn.execute(
                    "UPDATE a_task_rounds SET ended_at = ?1, status = 'stopped' WHERE status = 'running'",
                    params![now],
                );
                match result {
                    Ok(n) if n > 0 => eprintln!("[db] 跨日重置: 关闭了 {} 个 running 轮次", n),
                    Err(e) => eprintln!("[db] close_all_running_rounds 失败: {}", e),
                    _ => {},
                }
            })
            .await;
    }

    /// 跨日重置：关闭所有未结束的 run（running/paused → stopped）
    pub async fn close_all_unfinished_runs(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(|conn| {
                let now = now_unix();
                let result = conn.execute(
                    "UPDATE a_task_runs
                     SET ended_at = ?1,
                         duration_sec = ?1 - started_at,
                         status = 'stopped'
                     WHERE status IN ('running', 'paused')",
                    params![now],
                );
                match result {
                    Ok(n) if n > 0 => eprintln!("[db] 跨日重置: 关闭了 {} 条未结束 run", n),
                    Err(e) => eprintln!("[db] close_all_unfinished_runs 失败: {}", e),
                    _ => {},
                }
            })
            .await;
    }

    /// 跨日重置：清除已同步的进度记录，保留未上报的
    pub async fn cleanup_synced_progress(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(|conn| {
                let result =
                    conn.execute("DELETE FROM a_task_progress WHERE sync_status = 'synced'", []);
                match result {
                    Ok(n) if n > 0 => eprintln!("[db] 跨日重置: 清理了 {} 条已同步进度", n),
                    Err(e) => eprintln!("[db] cleanup_synced_progress 失败: {}", e),
                    _ => {},
                }
            })
            .await;
    }
}
