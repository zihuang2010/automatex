use super::{log_exec, now_unix, today_str, Database};
use rusqlite::params;
use serde::{Deserialize, Serialize};

// ─── 数据结构 ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunStats {
    pub last_run_at: Option<i64>,
    pub today_runs: i32,
    pub today_duration_sec: i64,
    pub today_keywords: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressRow {
    pub task_id: String,
    pub city_name: String,
    pub keyword_name: String,
    pub status: String,
    pub completed_at: i64,
    pub device_serial: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyStatRow {
    pub task_id: String,
    pub run_count: i32,
    pub total_duration_sec: i64,
    pub cities_done: i32,
    pub keywords_done: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailySummary {
    pub run_date: String,
    pub total_runs: i32,
    pub total_duration_sec: i64,
    pub total_cities_done: i32,
    pub total_keywords_done: i32,
}

// ─── 执行记录操作 ──────────────────────────────────────────────

impl Database {
    pub async fn start_task_run(&self, task_id: &str, device_serial: &str, round_id: i64) -> i64 {
        let task_id = task_id.to_string();
        let device_serial = device_serial.to_string();
        let Ok(conn) = self.pool.get().await else { return now_unix() };
        conn.interact(move |conn| {
            let now = now_unix();
            let today = today_str();
            let baseline: i32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM a_task_progress WHERE task_id = ?1 AND round_id = ?2",
                    params![task_id, round_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            log_exec(
                conn.execute(
                    "INSERT INTO a_task_runs
                        (task_id, device_serial, round_id, run_date, started_at, status, sync_status, keywords_baseline)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'running', 'pending', ?6)",
                    params![task_id, device_serial, round_id, today, now, baseline],
                ),
                "start_task_run",
            );
            now
        }).await.unwrap_or_else(|_| now_unix())
    }

    pub async fn finish_task_run(&self, task_id: &str, started_at: i64, status: &str) {
        let task_id = task_id.to_string();
        let status = status.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(move |conn| {
            let now = now_unix();
            let duration = now - started_at;
            log_exec(
                conn.execute(
                    "UPDATE a_task_runs
                     SET ended_at = ?1, duration_sec = ?2, status = ?3,
                         keywords_done = (SELECT COUNT(*) FROM a_task_progress WHERE task_id = ?4 AND round_id = a_task_runs.round_id) - keywords_baseline,
                         cities_done = (SELECT COUNT(DISTINCT city_name) FROM a_task_progress WHERE task_id = ?4 AND round_id = a_task_runs.round_id)
                     WHERE task_id = ?4 AND started_at = ?5",
                    params![now, duration, status, task_id, started_at],
                ),
                "finish_task_run",
            );
        }).await;
    }

    pub async fn cleanup_stale_assignments(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(|conn| {
                let paused = crate::constants::task_status::PAUSED;
                let executing = crate::constants::task_status::EXECUTING;
                let affected = conn.execute(
                    "UPDATE a_task_state SET status = ?1, assigned_device = NULL WHERE status = ?2",
                    params![paused, executing],
                );
                match affected {
                    Ok(n) if n > 0 => eprintln!("[db] 清理了 {} 条残留 EXECUTING 任务", n),
                    Err(e) => eprintln!("[db] cleanup_stale_assignments 失败: {}", e),
                    _ => {},
                }
            })
            .await;
    }

    pub async fn cleanup_orphan_runs(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(|conn| {
            let now = now_unix();
            let crashed = crate::constants::run_status::CRASHED;
            let running = crate::constants::run_status::RUNNING;
            let affected = conn.execute(
                "UPDATE a_task_runs
                 SET ended_at = ?1,
                     duration_sec = ?1 - started_at,
                     status = ?2,
                     keywords_done = COALESCE(
                         (SELECT COUNT(*) FROM a_task_progress WHERE task_id = a_task_runs.task_id AND round_id = a_task_runs.round_id) - keywords_baseline,
                         0
                     )
                 WHERE status = ?3",
                params![now, crashed, running],
            );
            match affected {
                Ok(n) if n > 0 => eprintln!("[db] 清理了 {} 条孤儿 run 记录", n),
                Err(e) => eprintln!("[db] cleanup_orphan_runs 失败: {}", e),
                _ => {},
            }
        }).await;
    }

    // ─── 统计查询 ──────────────────────────────────────────────

    pub async fn query_daily_stats(
        &self,
        device_serial: &str,
        run_date: &str,
    ) -> Vec<DailyStatRow> {
        let device_serial = device_serial.to_string();
        let run_date = run_date.to_string();
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT task_id, COUNT(*) as runs, COALESCE(SUM(duration_sec), 0) as total_sec,
                        COALESCE(SUM(cities_done), 0), COALESCE(SUM(keywords_done), 0)
                 FROM a_task_runs
                 WHERE device_serial = ?1 AND run_date = ?2
                 GROUP BY task_id",
            )?;
            let rows = stmt.query_map(params![device_serial, run_date], |row| {
                Ok(DailyStatRow {
                    task_id: row.get(0)?,
                    run_count: row.get(1)?,
                    total_duration_sec: row.get(2)?,
                    cities_done: row.get(3)?,
                    keywords_done: row.get(4)?,
                })
            })?;
            Ok::<_, rusqlite::Error>(rows.filter_map(|r| r.ok()).collect())
        })
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default()
    }

    pub async fn query_daily_summary(&self, run_date: &str) -> DailySummary {
        let run_date = run_date.to_string();
        let Ok(conn) = self.pool.get().await else {
            return DailySummary {
                run_date,
                total_runs: 0,
                total_duration_sec: 0,
                total_cities_done: 0,
                total_keywords_done: 0,
            };
        };
        let rd = run_date.clone();
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT COUNT(*) as total_runs,
                        COALESCE(SUM(duration_sec), 0),
                        COALESCE(SUM(cities_done), 0),
                        COALESCE(SUM(keywords_done), 0)
                 FROM a_task_runs WHERE run_date = ?1",
                params![rd],
                |row| {
                    Ok(DailySummary {
                        run_date: rd.clone(),
                        total_runs: row.get(0)?,
                        total_duration_sec: row.get(1)?,
                        total_cities_done: row.get(2)?,
                        total_keywords_done: row.get(3)?,
                    })
                },
            )
            .unwrap_or(DailySummary {
                run_date: rd,
                total_runs: 0,
                total_duration_sec: 0,
                total_cities_done: 0,
                total_keywords_done: 0,
            })
        })
        .await
        .unwrap_or(DailySummary {
            run_date,
            total_runs: 0,
            total_duration_sec: 0,
            total_cities_done: 0,
            total_keywords_done: 0,
        })
    }

    pub async fn query_task_run_stats(&self, task_id: &str) -> TaskRunStats {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else {
            return TaskRunStats {
                last_run_at: None,
                today_runs: 0,
                today_duration_sec: 0,
                today_keywords: 0,
            };
        };
        conn.interact(move |conn| {
            let today = today_str();
            let last_run_at: Option<i64> = conn
                .query_row(
                    "SELECT started_at FROM a_task_runs WHERE task_id = ?1 ORDER BY started_at DESC LIMIT 1",
                    params![task_id], |row| row.get(0),
                ).ok();
            let today_runs: i32 = conn
                .query_row("SELECT COUNT(*) FROM a_task_runs WHERE task_id = ?1 AND run_date = ?2",
                    params![task_id, today], |row| row.get(0)).unwrap_or(0);
            let today_duration_sec: i64 = conn
                .query_row("SELECT COALESCE(SUM(duration_sec), 0) FROM a_task_runs WHERE task_id = ?1 AND run_date = ?2",
                    params![task_id, today], |row| row.get(0)).unwrap_or(0);
            let today_keywords: i32 = conn
                .query_row("SELECT COALESCE(SUM(keywords_done), 0) FROM a_task_runs WHERE task_id = ?1 AND run_date = ?2",
                    params![task_id, today], |row| row.get(0)).unwrap_or(0);
            TaskRunStats { last_run_at, today_runs, today_duration_sec, today_keywords }
        }).await.unwrap_or(TaskRunStats { last_run_at: None, today_runs: 0, today_duration_sec: 0, today_keywords: 0 })
    }
}
