use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

use crate::constants;

/// 数据库中的设备行（包含静态 + 动态属性）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRow {
    pub serial: String,
    pub hw_serial: String,
    pub name: String,
    pub device_type: String,
    pub address: Option<String>,
    pub state: String,
    pub model: String,
    pub brand: String,
    pub android_version: String,
    pub sdk_version: String,
    pub display_resolution: String,
    pub battery_level: i32,
    pub battery_temperature: f64,
    pub updated_at: i64,
}

/// SQLite 持久化数据库（读写分离，WAL 模式下支持并发读写）
pub struct Database {
    writer: Mutex<Connection>,
    reader: Mutex<Connection>,
}

// FIX #6: DB 错误日志化（替代 let _ = ...）
fn log_exec(result: rusqlite::Result<usize>, op: &str) {
    if let Err(e) = result {
        eprintln!("[db] {} 失败: {}", op, e);
    }
}

impl Database {
    // FIX #5: Mutex 安全恢复（不会因 poison 导致整个应用崩溃）
    fn w(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.writer.lock().unwrap_or_else(|e| {
            eprintln!("[db] Writer Mutex poisoned, recovering");
            e.into_inner()
        })
    }

    fn r(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.reader.lock().unwrap_or_else(|e| {
            eprintln!("[db] Reader Mutex poisoned, recovering");
            e.into_inner()
        })
    }

    /// 初始化数据库（创建/打开 + 建表 + 迁移）
    pub fn init(app_data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(app_data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;

        let db_path = app_data_dir.join("automatex.db");
        let writer = Connection::open(&db_path).map_err(|e| format!("打开数据库失败: {}", e))?;

        writer
            .execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| format!("设置 WAL 失败: {}", e))?;

        writer
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS a_devices (
                serial             TEXT PRIMARY KEY,
                hw_serial          TEXT NOT NULL DEFAULT '',
                name               TEXT NOT NULL DEFAULT '',
                device_type        TEXT NOT NULL DEFAULT 'usb',
                address            TEXT,
                state              TEXT NOT NULL DEFAULT 'Offline',
                model              TEXT NOT NULL DEFAULT 'unknown',
                brand              TEXT NOT NULL DEFAULT 'unknown',
                android_version    TEXT NOT NULL DEFAULT 'unknown',
                sdk_version        TEXT NOT NULL DEFAULT 'unknown',
                display_resolution TEXT NOT NULL DEFAULT 'unknown',
                battery_level      INTEGER NOT NULL DEFAULT 0,
                battery_temperature REAL NOT NULL DEFAULT 0.0,
                updated_at         INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS a_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS a_task_cache (
                task_id    TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                payload    TEXT NOT NULL,
                version    INTEGER NOT NULL DEFAULT 1,
                fetched_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS a_task_progress (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id       TEXT NOT NULL,
                city_name     TEXT NOT NULL,
                keyword_name  TEXT NOT NULL,
                status        TEXT NOT NULL DEFAULT 'ok',
                completed_at  INTEGER NOT NULL,
                device_serial TEXT NOT NULL,
                sync_status   TEXT NOT NULL DEFAULT 'pending',
                UNIQUE(task_id, city_name, keyword_name)
            );

            CREATE TABLE IF NOT EXISTS a_task_runs (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id       TEXT NOT NULL,
                device_serial TEXT NOT NULL,
                run_date      TEXT NOT NULL,
                started_at    INTEGER NOT NULL,
                ended_at      INTEGER,
                duration_sec  INTEGER,
                status        TEXT NOT NULL DEFAULT 'running',
                cities_done   INTEGER NOT NULL DEFAULT 0,
                keywords_done INTEGER NOT NULL DEFAULT 0,
                sync_status   TEXT NOT NULL DEFAULT 'pending',
                UNIQUE(task_id, device_serial, started_at)
            );

            CREATE INDEX IF NOT EXISTS idx_progress_task ON a_task_progress(task_id);
            CREATE INDEX IF NOT EXISTS idx_progress_sync ON a_task_progress(sync_status);
            CREATE INDEX IF NOT EXISTS idx_runs_date ON a_task_runs(run_date, task_id);
            CREATE INDEX IF NOT EXISTS idx_runs_sync ON a_task_runs(sync_status);",
            )
            .map_err(|e| format!("建表失败: {}", e))?;

        let reader = Connection::open(&db_path).map_err(|e| format!("打开读连接失败: {}", e))?;

        let _ = writer.execute_batch(
            "ALTER TABLE a_task_cache ADD COLUMN status TEXT NOT NULL DEFAULT 'WAITING';
             ALTER TABLE a_task_cache ADD COLUMN assigned_device TEXT;",
        );

        // 迁移：添加 keywords_baseline 列（记录每次 run 开始时的基线）
        let _ = writer.execute_batch(
            "ALTER TABLE a_task_runs ADD COLUMN keywords_baseline INTEGER NOT NULL DEFAULT 0;",
        );

        Ok(Self { writer: Mutex::new(writer), reader: Mutex::new(reader) })
    }

    // ─── 设备写操作 ────────────────────────────────────────────

    pub fn upsert_device(&self, row: &DeviceRow) {
        let conn = self.w();
        log_exec(
            conn.execute(
                "INSERT OR REPLACE INTO a_devices
                    (serial, hw_serial, name, device_type, address, state,
                     model, brand, android_version, sdk_version, display_resolution,
                     battery_level, battery_temperature, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    row.serial,
                    row.hw_serial,
                    row.name,
                    row.device_type,
                    row.address,
                    row.state,
                    row.model,
                    row.brand,
                    row.android_version,
                    row.sdk_version,
                    row.display_resolution,
                    row.battery_level,
                    row.battery_temperature,
                    row.updated_at,
                ],
            ),
            "upsert_device",
        );
    }

    pub fn update_device_state(&self, serial: &str, state: &str) {
        let conn = self.w();
        let now = now_unix();
        log_exec(
            conn.execute(
                "UPDATE a_devices SET state = ?1, updated_at = ?2 WHERE serial = ?3",
                params![state, now, serial],
            ),
            "update_device_state",
        );
    }

    pub fn update_device_props(&self, serial: &str, battery_level: i32, battery_temperature: f64) {
        let conn = self.w();
        let now = now_unix();
        log_exec(
            conn.execute(
                "UPDATE a_devices SET battery_level = ?1, battery_temperature = ?2, updated_at = ?3
                 WHERE serial = ?4",
                params![battery_level, battery_temperature, now, serial],
            ),
            "update_device_props",
        );
    }

    pub fn mark_offline_except(&self, online_serials: &[&str]) {
        let conn = self.w();
        let now = now_unix();
        if online_serials.is_empty() {
            log_exec(
                conn.execute(
                    "UPDATE a_devices SET state = 'Offline', updated_at = ?1 WHERE state != 'Offline'",
                    params![now],
                ),
                "mark_all_offline",
            );
        } else {
            let placeholders: Vec<String> =
                (0..online_serials.len()).map(|i| format!("?{}", i + 2)).collect();
            let sql = format!(
                "UPDATE a_devices SET state = 'Offline', updated_at = ?1 WHERE state != 'Offline' AND serial NOT IN ({})",
                placeholders.join(",")
            );
            let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
                Vec::with_capacity(online_serials.len() + 1);
            param_values.push(Box::new(now));
            for s in online_serials {
                param_values.push(Box::new(s.to_string()));
            }
            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                param_values.iter().map(|p| p.as_ref()).collect();
            log_exec(conn.execute(&sql, params_ref.as_slice()), "mark_offline_except");
        }
    }

    pub fn delete_device(&self, serial: &str) {
        let conn = self.w();
        log_exec(
            conn.execute("DELETE FROM a_devices WHERE serial = ?1", params![serial]),
            "delete_device",
        );
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let conn = self.w();
        log_exec(
            conn.execute(
                "INSERT OR REPLACE INTO a_settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            ),
            "set_setting",
        );
    }

    // ─── 设备读操作（使用独立读连接，不阻塞写入）─────────────

    pub fn device_exists(&self, serial: &str) -> bool {
        let conn = self.r();
        conn.query_row("SELECT 1 FROM a_devices WHERE serial = ?1", params![serial], |_| Ok(()))
            .is_ok()
    }

    pub fn needs_prop_refresh(&self, serial: &str) -> bool {
        let conn = self.r();
        conn.query_row("SELECT model FROM a_devices WHERE serial = ?1", params![serial], |row| {
            row.get::<_, String>(0)
        })
        .map(|m| m == constants::device_state::UNKNOWN)
        .unwrap_or(true)
    }

    pub fn load_all_devices(&self) -> Vec<DeviceRow> {
        let conn = self.r();
        let mut stmt = match conn.prepare(
            "SELECT serial, hw_serial, name, device_type, address, state,
                    model, brand, android_version, sdk_version, display_resolution,
                    battery_level, battery_temperature, updated_at
             FROM a_devices
             ORDER BY CASE state WHEN 'Offline' THEN 1 ELSE 0 END, name",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        stmt.query_map([], |row| {
            Ok(DeviceRow {
                serial: row.get(0)?,
                hw_serial: row.get(1)?,
                name: row.get(2)?,
                device_type: row.get(3)?,
                address: row.get(4)?,
                state: row.get(5)?,
                model: row.get(6)?,
                brand: row.get(7)?,
                android_version: row.get(8)?,
                sdk_version: row.get(9)?,
                display_resolution: row.get(10)?,
                battery_level: row.get(11)?,
                battery_temperature: row.get(12)?,
                updated_at: row.get(13)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    pub fn get_device_by_serial(&self, serial: &str) -> Option<DeviceRow> {
        let conn = self.r();
        conn.query_row(
            "SELECT serial, hw_serial, name, device_type, address, state,
                    model, brand, android_version, sdk_version, display_resolution,
                    battery_level, battery_temperature, updated_at
             FROM a_devices WHERE serial = ?1",
            params![serial],
            |row| {
                Ok(DeviceRow {
                    serial: row.get(0)?,
                    hw_serial: row.get(1)?,
                    name: row.get(2)?,
                    device_type: row.get(3)?,
                    address: row.get(4)?,
                    state: row.get(5)?,
                    model: row.get(6)?,
                    brand: row.get(7)?,
                    android_version: row.get(8)?,
                    sdk_version: row.get(9)?,
                    display_resolution: row.get(10)?,
                    battery_level: row.get(11)?,
                    battery_temperature: row.get(12)?,
                    updated_at: row.get(13)?,
                })
            },
        )
        .ok()
    }

    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.r();
        conn.query_row("SELECT value FROM a_settings WHERE key = ?1", params![key], |row| {
            row.get(0)
        })
        .ok()
    }

    // ─── 任务缓存操作 ────────────────────────────────────────────

    pub fn upsert_task_cache(&self, task_id: &str, name: &str, payload: &str, version: i64) {
        let conn = self.w();
        let now = now_unix();
        log_exec(
            conn.execute(
                "INSERT INTO a_task_cache (task_id, name, payload, version, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(task_id) DO UPDATE SET name=excluded.name, payload=excluded.payload, version=excluded.version, fetched_at=excluded.fetched_at",
                params![task_id, name, payload, version, now],
            ),
            "upsert_task_cache",
        );
    }

    pub fn save_task_state(&self, task_id: &str, status: &str, assigned_device: Option<&str>) {
        let conn = self.w();
        log_exec(
            conn.execute(
                "UPDATE a_task_cache SET status = ?2, assigned_device = ?3 WHERE task_id = ?1",
                params![task_id, status, assigned_device],
            ),
            "save_task_state",
        );
    }

    pub fn load_task_state(&self, task_id: &str) -> Option<(String, Option<String>)> {
        let conn = self.r();
        conn.query_row(
            "SELECT status, assigned_device FROM a_task_cache WHERE task_id = ?1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
    }

    pub fn delete_task_state(&self, task_id: &str) {
        let conn = self.w();
        log_exec(
            conn.execute(
                "UPDATE a_task_cache SET status = 'WAITING', assigned_device = NULL WHERE task_id = ?1",
                params![task_id],
            ),
            "delete_task_state",
        );
    }

    #[allow(dead_code)]
    pub fn load_task_cache(&self, task_id: &str) -> Option<TaskCacheRow> {
        let conn = self.r();
        conn.query_row(
            "SELECT task_id, name, payload, version, fetched_at FROM a_task_cache WHERE task_id = ?1",
            params![task_id],
            |row| {
                Ok(TaskCacheRow {
                    task_id: row.get(0)?, name: row.get(1)?, payload: row.get(2)?,
                    version: row.get(3)?, fetched_at: row.get(4)?,
                })
            },
        )
        .ok()
    }

    #[allow(dead_code)]
    pub fn load_all_task_caches(&self) -> Vec<TaskCacheRow> {
        let conn = self.r();
        let mut stmt = match conn.prepare(
            "SELECT task_id, name, payload, version, fetched_at FROM a_task_cache ORDER BY name",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |row| {
            Ok(TaskCacheRow {
                task_id: row.get(0)?,
                name: row.get(1)?,
                payload: row.get(2)?,
                version: row.get(3)?,
                fetched_at: row.get(4)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn delete_task_cache(&self, task_id: &str) {
        let conn = self.w();
        log_exec(
            conn.execute("DELETE FROM a_task_cache WHERE task_id = ?1", params![task_id]),
            "delete_task_cache",
        );
    }

    // ─── 执行进度操作 ────────────────────────────────────────────

    pub fn record_keyword_done(
        &self,
        task_id: &str,
        city_name: &str,
        keyword_name: &str,
        device_serial: &str,
    ) {
        let conn = self.w();
        let now = now_unix();
        log_exec(
            conn.execute(
                "INSERT OR IGNORE INTO a_task_progress
                    (task_id, city_name, keyword_name, status, completed_at, device_serial, sync_status)
                 VALUES (?1, ?2, ?3, 'ok', ?4, ?5, 'pending')",
                params![task_id, city_name, keyword_name, now, device_serial],
            ),
            "record_keyword_done",
        );
    }

    pub fn load_task_progress(&self, task_id: &str) -> Vec<ProgressRow> {
        let conn = self.r();
        let mut stmt = match conn.prepare(
            "SELECT task_id, city_name, keyword_name, status, completed_at, device_serial
             FROM a_task_progress WHERE task_id = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![task_id], |row| {
            Ok(ProgressRow {
                task_id: row.get(0)?,
                city_name: row.get(1)?,
                keyword_name: row.get(2)?,
                status: row.get(3)?,
                completed_at: row.get(4)?,
                device_serial: row.get(5)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    pub fn clear_task_progress(&self, task_id: &str) {
        let conn = self.w();
        log_exec(
            conn.execute("DELETE FROM a_task_progress WHERE task_id = ?1", params![task_id]),
            "clear_task_progress",
        );
    }

    #[allow(dead_code)]
    pub fn load_pending_progress(&self) -> Vec<ProgressRow> {
        let conn = self.r();
        let mut stmt = match conn.prepare(
            "SELECT task_id, city_name, keyword_name, status, completed_at, device_serial
             FROM a_task_progress WHERE sync_status = 'pending'",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |row| {
            Ok(ProgressRow {
                task_id: row.get(0)?,
                city_name: row.get(1)?,
                keyword_name: row.get(2)?,
                status: row.get(3)?,
                completed_at: row.get(4)?,
                device_serial: row.get(5)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn mark_progress_synced(&self, task_id: &str, city_name: &str, keyword_name: &str) {
        let conn = self.w();
        log_exec(
            conn.execute(
                "UPDATE a_task_progress SET sync_status = 'synced'
                 WHERE task_id = ?1 AND city_name = ?2 AND keyword_name = ?3",
                params![task_id, city_name, keyword_name],
            ),
            "mark_progress_synced",
        );
    }

    // ─── 执行记录操作 ────────────────────────────────────────────

    /// 开始一次执行记录（同时记录当前进度基线，用于计算增量）
    pub fn start_task_run(&self, task_id: &str, device_serial: &str) -> i64 {
        let conn = self.w();
        let now = now_unix();
        let today = today_str();

        // 记录开始时的进度基线
        let baseline: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM a_task_progress WHERE task_id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        log_exec(
            conn.execute(
                "INSERT INTO a_task_runs
                    (task_id, device_serial, run_date, started_at, status, sync_status, keywords_baseline)
                 VALUES (?1, ?2, ?3, ?4, 'running', 'pending', ?5)",
                params![task_id, device_serial, today, now, baseline],
            ),
            "start_task_run",
        );
        now
    }

    #[allow(dead_code)]
    pub fn update_task_run_stats(
        &self,
        task_id: &str,
        started_at: i64,
        cities_done: i32,
        keywords_done: i32,
    ) {
        let conn = self.w();
        log_exec(
            conn.execute(
                "UPDATE a_task_runs SET cities_done = ?1, keywords_done = ?2
                 WHERE task_id = ?3 AND started_at = ?4",
                params![cities_done, keywords_done, task_id, started_at],
            ),
            "update_task_run_stats",
        );
    }

    /// keywords_done = 当前总进度 - 基线 = 本轮增量
    pub fn finish_task_run(&self, task_id: &str, started_at: i64, status: &str) {
        let conn = self.w();
        let now = now_unix();
        let duration = now - started_at;
        log_exec(
            conn.execute(
                "UPDATE a_task_runs
                 SET ended_at = ?1, duration_sec = ?2, status = ?3,
                     keywords_done = (SELECT COUNT(*) FROM a_task_progress WHERE task_id = ?4) - keywords_baseline,
                     cities_done = (SELECT COUNT(DISTINCT city_name) FROM a_task_progress WHERE task_id = ?4)
                 WHERE task_id = ?4 AND started_at = ?5",
                params![now, duration, status, task_id, started_at],
            ),
            "finish_task_run",
        );
    }

    /// 启动时清理孤儿 run（崩溃/异常退出导致 status 仍为 running 的记录）
    pub fn cleanup_orphan_runs(&self) {
        let conn = self.w();
        let now = now_unix();
        let affected = conn.execute(
            "UPDATE a_task_runs
             SET ended_at = ?1,
                 duration_sec = ?1 - started_at,
                 status = 'crashed',
                 keywords_done = COALESCE(
                     (SELECT COUNT(*) FROM a_task_progress WHERE task_id = a_task_runs.task_id) - keywords_baseline,
                     0
                 )
             WHERE status = 'running'",
            params![now],
        );
        match affected {
            Ok(n) if n > 0 => eprintln!("[db] 清理了 {} 条孤儿 run 记录", n),
            Err(e) => eprintln!("[db] cleanup_orphan_runs 失败: {}", e),
            _ => {},
        }
    }

    pub fn query_daily_stats(&self, device_serial: &str, run_date: &str) -> Vec<DailyStatRow> {
        let conn = self.r();
        let mut stmt = match conn.prepare(
            "SELECT task_id, COUNT(*) as runs, COALESCE(SUM(duration_sec), 0) as total_sec,
                    COALESCE(SUM(cities_done), 0), COALESCE(SUM(keywords_done), 0)
             FROM a_task_runs
             WHERE device_serial = ?1 AND run_date = ?2
             GROUP BY task_id",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![device_serial, run_date], |row| {
            Ok(DailyStatRow {
                task_id: row.get(0)?,
                run_count: row.get(1)?,
                total_duration_sec: row.get(2)?,
                cities_done: row.get(3)?,
                keywords_done: row.get(4)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    pub fn query_daily_summary(&self, run_date: &str) -> DailySummary {
        let conn = self.r();
        conn.query_row(
            "SELECT COUNT(*) as total_runs,
                    COALESCE(SUM(duration_sec), 0),
                    COALESCE(SUM(cities_done), 0),
                    COALESCE(SUM(keywords_done), 0)
             FROM a_task_runs WHERE run_date = ?1",
            params![run_date],
            |row| {
                Ok(DailySummary {
                    run_date: run_date.to_string(),
                    total_runs: row.get(0)?,
                    total_duration_sec: row.get(1)?,
                    total_cities_done: row.get(2)?,
                    total_keywords_done: row.get(3)?,
                })
            },
        )
        .unwrap_or(DailySummary {
            run_date: run_date.to_string(),
            total_runs: 0,
            total_duration_sec: 0,
            total_cities_done: 0,
            total_keywords_done: 0,
        })
    }

    pub fn query_task_run_stats(&self, task_id: &str) -> TaskRunStats {
        let conn = self.r();
        let today = today_str();
        let last_run_at: Option<i64> = conn
            .query_row(
                "SELECT started_at FROM a_task_runs WHERE task_id = ?1 ORDER BY started_at DESC LIMIT 1",
                params![task_id],
                |row| row.get(0),
            )
            .ok();
        let today_runs: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM a_task_runs WHERE task_id = ?1 AND run_date = ?2",
                params![task_id, today],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let today_duration_sec: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(duration_sec), 0) FROM a_task_runs WHERE task_id = ?1 AND run_date = ?2",
                params![task_id, today],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let today_keywords: i32 = conn
            .query_row(
                "SELECT COALESCE(SUM(keywords_done), 0) FROM a_task_runs WHERE task_id = ?1 AND run_date = ?2",
                params![task_id, today],
                |row| row.get(0),
            )
            .unwrap_or(0);

        TaskRunStats { last_run_at, today_runs, today_duration_sec, today_keywords }
    }
}

// ─── 数据结构 ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunStats {
    pub last_run_at: Option<i64>,
    pub today_runs: i32,
    pub today_duration_sec: i64,
    pub today_keywords: i32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCacheRow {
    pub task_id: String,
    pub name: String,
    pub payload: String,
    pub version: i64,
    pub fetched_at: i64,
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

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
        as i64
}

fn today_str() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}
