mod devices;
mod progress;
mod settings;
mod stats;
mod tasks;

pub use stats::{DailyStatRow, DailySummary, ProgressRow, TaskRunStats};

use deadpool_sqlite::{Config, Hook, Pool, Runtime};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

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
    pub is_flagged: bool,
    pub updated_at: i64,
}

/// SQLite 持久化数据库（deadpool-sqlite 异步连接池）
pub struct Database {
    pub(crate) pool: Pool,
}

// ─── 辅助函数 ──────────────────────────────────────────────────

pub(crate) fn log_exec(result: rusqlite::Result<usize>, op: &str) {
    if let Err(e) = result {
        eprintln!("[db] {} 失败: {}", op, e);
    }
}

/// 从 SQL Row 构建 DeviceRow（消除重复映射代码）
pub(crate) fn row_to_device(row: &rusqlite::Row) -> rusqlite::Result<DeviceRow> {
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
        is_flagged: row.get::<_, i32>(13).unwrap_or(0) != 0,
        updated_at: row.get(14)?,
    })
}

pub(crate) fn now_unix() -> i64 {
    crate::constants::now_unix()
}

pub(crate) fn today_str() -> String {
    crate::constants::today_str()
}

impl Database {
    /// 初始化数据库（同步：建表 + 迁移用裸 Connection，然后创建 Pool）
    pub fn init(app_data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(app_data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;

        let db_path = app_data_dir.join("automatex.db");
        let db_path_str = db_path
            .to_str()
            .ok_or_else(|| "数据库路径包含非 UTF-8 字符".to_string())?
            .to_string();

        // 同步建表（裸 Connection，一次性操作）
        {
            let conn = Connection::open(&db_path).map_err(|e| format!("打开数据库失败: {}", e))?;
            conn.execute_batch("PRAGMA journal_mode=WAL;")
                .map_err(|e| format!("设置 WAL 失败: {}", e))?;

            conn.execute_batch(
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
                is_flagged         INTEGER NOT NULL DEFAULT 0,
                updated_at         INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS a_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS a_task_defs (
                task_id    TEXT PRIMARY KEY,
                name       TEXT NOT NULL DEFAULT '',
                payload    TEXT NOT NULL DEFAULT '[]',
                version    INTEGER NOT NULL DEFAULT 1,
                fetched_at INTEGER NOT NULL DEFAULT 0,
                city_order TEXT,
                phone      TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS a_task_state (
                task_id          TEXT PRIMARY KEY,
                status           TEXT NOT NULL DEFAULT 'waiting',
                assigned_device  TEXT,
                current_round_id INTEGER
            );

            CREATE TABLE IF NOT EXISTS a_task_rounds (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id    TEXT NOT NULL,
                run_date   TEXT NOT NULL,
                round_no   INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                ended_at   INTEGER,
                status     TEXT NOT NULL DEFAULT 'running',
                UNIQUE(task_id, run_date, round_no)
            );

            CREATE TABLE IF NOT EXISTS a_task_runs (
                id                INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id           TEXT NOT NULL,
                device_serial     TEXT NOT NULL,
                round_id          INTEGER,
                run_date          TEXT NOT NULL,
                started_at        INTEGER NOT NULL,
                ended_at          INTEGER,
                duration_sec      INTEGER,
                status            TEXT NOT NULL DEFAULT 'running',
                cities_done       INTEGER NOT NULL DEFAULT 0,
                keywords_done     INTEGER NOT NULL DEFAULT 0,
                keywords_baseline INTEGER NOT NULL DEFAULT 0,
                cities_baseline   INTEGER NOT NULL DEFAULT 0,
                sync_status       TEXT NOT NULL DEFAULT 'pending'
            );

            CREATE TABLE IF NOT EXISTS a_task_progress (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id       TEXT NOT NULL,
                city_name     TEXT NOT NULL,
                keyword_name  TEXT NOT NULL,
                round_id      INTEGER NOT NULL DEFAULT 0,
                status        TEXT NOT NULL DEFAULT 'ok',
                completed_at  INTEGER NOT NULL,
                device_serial TEXT NOT NULL,
                sync_status   TEXT NOT NULL DEFAULT 'pending',
                UNIQUE(task_id, city_name, keyword_name, round_id)
            );

            CREATE INDEX IF NOT EXISTS idx_devices_hw_serial ON a_devices(hw_serial);
            CREATE INDEX IF NOT EXISTS idx_task_defs_phone ON a_task_defs(phone);
            CREATE INDEX IF NOT EXISTS idx_rounds_task ON a_task_rounds(task_id);
            CREATE INDEX IF NOT EXISTS idx_rounds_date ON a_task_rounds(run_date);
            CREATE INDEX IF NOT EXISTS idx_progress_task ON a_task_progress(task_id);
            CREATE INDEX IF NOT EXISTS idx_progress_round ON a_task_progress(round_id);
            CREATE INDEX IF NOT EXISTS idx_progress_sync ON a_task_progress(sync_status);
            CREATE INDEX IF NOT EXISTS idx_runs_task ON a_task_runs(task_id);
            CREATE INDEX IF NOT EXISTS idx_runs_round ON a_task_runs(round_id);
            CREATE INDEX IF NOT EXISTS idx_runs_date ON a_task_runs(run_date, task_id);
            CREATE INDEX IF NOT EXISTS idx_runs_sync ON a_task_runs(sync_status);
            CREATE INDEX IF NOT EXISTS idx_runs_device_date ON a_task_runs(device_serial, run_date);",
            )
            .map_err(|e| format!("建表失败: {}", e))?;

            // 迁移：为已有数据库添加新列（忽略 duplicate column 错误）
            let _ = conn.execute_batch(
                "ALTER TABLE a_task_runs ADD COLUMN cities_baseline INTEGER NOT NULL DEFAULT 0;",
            );
        }

        // Phase 2: 创建 deadpool-sqlite 连接池（带 PRAGMA hook + 限制池大小）
        let cfg = Config::new(&db_path_str);
        let pool = cfg
            .builder(Runtime::Tokio1)
            .map_err(|e| format!("创建连接池 builder 失败: {}", e))?
            .max_size(4)
            .post_create(Hook::async_fn(|conn, _| {
                Box::pin(async move {
                    conn.interact(|conn| {
                        conn.execute_batch(
                            "PRAGMA busy_timeout = 5000;
                             PRAGMA foreign_keys = ON;",
                        )
                    })
                    .await
                    .map_err(|e| {
                        deadpool_sqlite::HookError::message(format!("PRAGMA interact 失败: {}", e))
                    })?
                    .map_err(|e| {
                        deadpool_sqlite::HookError::message(format!("PRAGMA exec 失败: {}", e))
                    })?;
                    Ok(())
                })
            }))
            .build()
            .map_err(|e| format!("构建连接池失败: {}", e))?;

        Ok(Self { pool })
    }
}
