mod devices;
mod progress;
mod settings;
mod stats;
mod tasks;

pub use progress::ResultRow;
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskStateRow {
    pub status: String,
    pub assigned_device: Option<String>,
    pub current_round_id: Option<i64>,
    pub current_city_name: Option<String>,
    pub current_keyword_name: Option<String>,
    pub attempt: i32,
    pub next_wakeup_at: Option<i64>,
    pub last_error: Option<String>,
    pub runtime_status: Option<String>,
}

/// save_task_state 的参数包（消除 11 参数 code smell）
#[derive(Default)]
pub struct SaveTaskStateParams<'a> {
    pub task_id: &'a str,
    pub status: &'a str,
    pub assigned_device: Option<&'a str>,
    pub current_round_id: Option<i64>,
    pub current_city_name: Option<&'a str>,
    pub current_keyword_name: Option<&'a str>,
    pub attempt: i32,
    pub next_wakeup_at: Option<i64>,
    pub last_error: Option<&'a str>,
    pub runtime_status: Option<&'a str>,
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
                current_round_id INTEGER,
                current_city_name TEXT,
                current_keyword_name TEXT,
                attempt          INTEGER NOT NULL DEFAULT 0,
                next_wakeup_at   INTEGER,
                last_error       TEXT,
                runtime_status   TEXT
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
                item_count    INTEGER NOT NULL DEFAULT 0,
                UNIQUE(task_id, city_name, keyword_name, round_id)
            );

            CREATE TABLE IF NOT EXISTS a_task_results (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id      TEXT NOT NULL,
                round_id     INTEGER NOT NULL,
                city_name    TEXT NOT NULL,
                keyword_name TEXT NOT NULL,
                shop_name    TEXT NOT NULL,
                captured_at  TEXT NOT NULL DEFAULT '',
                created_at   INTEGER NOT NULL
            );",
            )
            .map_err(|e| format!("建表失败: {}", e))?;

            // ARC-1 修复（Part 1）：建立 _schema_version 档案表追踪迁移历史
            // 当前仍使用 ALTER TABLE 增量方式，但版本号提供可观测性，
            // 为后续迁移至 refinery/diesel_migrations 奠定基础。
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS _schema_version (
                    version     INTEGER PRIMARY KEY,
                    applied_at  INTEGER NOT NULL DEFAULT 0,
                    description TEXT
                );",
            )
            .map_err(|e| format!("建 _schema_version 表失败: {}", e))?;

            // ARC-1 修复（Part 2）：迁移循环改为 fail-loud 模式：
            // - "duplicate column" 错误：列已存在，静默跳过（兼容旧库）
            // - 其他错误：明确失败，防止 schema 静默不一致
            let versioned_migrations: &[(i64, &str, &str)] = &[
                (1, "ALTER TABLE a_task_progress ADD COLUMN round_id INTEGER NOT NULL DEFAULT 0;", "progress.round_id"),
                (2, "ALTER TABLE a_task_progress ADD COLUMN sync_status TEXT NOT NULL DEFAULT 'pending';", "progress.sync_status"),
                (3, "ALTER TABLE a_task_progress ADD COLUMN device_serial TEXT NOT NULL DEFAULT '';", "progress.device_serial"),
                (4, "ALTER TABLE a_task_runs ADD COLUMN round_id INTEGER;", "runs.round_id"),
                (5, "ALTER TABLE a_task_runs ADD COLUMN sync_status TEXT NOT NULL DEFAULT 'pending';", "runs.sync_status"),
                (6, "ALTER TABLE a_task_runs ADD COLUMN cities_baseline INTEGER NOT NULL DEFAULT 0;", "runs.cities_baseline"),
                (7, "ALTER TABLE a_task_runs ADD COLUMN keywords_baseline INTEGER NOT NULL DEFAULT 0;", "runs.keywords_baseline"),
                (8, "ALTER TABLE a_task_state ADD COLUMN current_city_name TEXT;", "state.current_city_name"),
                (9, "ALTER TABLE a_task_state ADD COLUMN current_keyword_name TEXT;", "state.current_keyword_name"),
                (10, "ALTER TABLE a_task_state ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0;", "state.attempt"),
                (11, "ALTER TABLE a_task_state ADD COLUMN next_wakeup_at INTEGER;", "state.next_wakeup_at"),
                (12, "ALTER TABLE a_task_state ADD COLUMN last_error TEXT;", "state.last_error"),
                (13, "ALTER TABLE a_task_state ADD COLUMN runtime_status TEXT;", "state.runtime_status"),
                (14, "ALTER TABLE a_devices ADD COLUMN local_port INTEGER;", "devices.local_port"),
                (15, "ALTER TABLE a_task_progress ADD COLUMN item_count INTEGER NOT NULL DEFAULT 0;", "progress.item_count"),
            ];

            let now_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            for (version, sql, description) in versioned_migrations {
                match conn.execute_batch(sql) {
                    Ok(_) => {
                        // 迁移成功，记录版本
                        let _ = conn.execute(
                            "INSERT OR IGNORE INTO _schema_version (version, applied_at, description) VALUES (?1, ?2, ?3)",
                            rusqlite::params![version, now_ts, description],
                        );
                        eprintln!("[db] 迁移 v{} ({}) 已应用", version, description);
                    },
                    Err(e) => {
                        let err_msg = e.to_string();
                        if err_msg.contains("duplicate column") {
                            // 列已存在（旧库已应用此迁移），静默记录版本
                            let _ = conn.execute(
                                "INSERT OR IGNORE INTO _schema_version (version, applied_at, description) VALUES (?1, ?2, ?3)",
                                rusqlite::params![version, now_ts, description],
                            );
                        } else {
                            // ARC-1 fail-loud：真实迁移错误，返回 Err 而非静默吞掉
                            return Err(format!(
                                "[db] 迁移 v{} ({}) 失败: {}",
                                version, description, e
                            ));
                        }
                    },
                }
            }

            // 建索引（此时所有列已确保存在）
            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_devices_hw_serial ON a_devices(hw_serial);
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
            CREATE INDEX IF NOT EXISTS idx_runs_device_date ON a_task_runs(device_serial, run_date);
            CREATE INDEX IF NOT EXISTS idx_results_lookup ON a_task_results(task_id, city_name, keyword_name);
            CREATE INDEX IF NOT EXISTS idx_results_round ON a_task_results(task_id, round_id);
            CREATE INDEX IF NOT EXISTS idx_results_round_id ON a_task_results(round_id);",
            )
            .map_err(|e| format!("建索引失败: {}", e))?;
        }

        // Phase 2: 创建 deadpool-sqlite 连接池（带 PRAGMA hook + 限制池大小）
        let cfg = Config::new(&db_path_str);
        let pool = cfg
            .builder(Runtime::Tokio1)
            .map_err(|e| format!("创建连接池 builder 失败: {}", e))?
            .max_size(8) // R7 优化：从 4 提升到 8，减少高并发 SQLITE_BUSY
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
