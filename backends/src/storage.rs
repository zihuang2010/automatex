use deadpool_sqlite::{Config, Hook, Pool, Runtime};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

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
    pub is_flagged: bool,
    pub updated_at: i64,
}

/// SQLite 持久化数据库（deadpool-sqlite 异步连接池）
// TODO #12: 任务/城市/关键词状态用字符串常量比较，后续改为 enum + serde(rename) 提升类型安全。
pub struct Database {
    pool: Pool,
}

// ─── 辅助函数 ──────────────────────────────────────────────────

fn log_exec(result: rusqlite::Result<usize>, op: &str) {
    if let Err(e) = result {
        eprintln!("[db] {} 失败: {}", op, e);
    }
}

/// 从 SQL Row 构建 DeviceRow（消除重复映射代码）
fn row_to_device(row: &rusqlite::Row) -> rusqlite::Result<DeviceRow> {
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

/// deadpool-sqlite interact 统一错误转换
#[allow(dead_code)]
fn map_interact_err(e: deadpool_sqlite::InteractError) -> String {
    format!("DB interact error: {}", e)
}

#[allow(dead_code)]
fn map_pool_err(e: deadpool_sqlite::PoolError) -> String {
    format!("DB pool error: {}", e)
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
                status           TEXT NOT NULL DEFAULT 'WAITING',
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

    // ─── 设备写操作 ────────────────────────────────────────────

    pub async fn upsert_device(&self, row: &DeviceRow) {
        let row = row.clone();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "INSERT INTO a_devices
                        (serial, hw_serial, name, device_type, address, state,
                         model, brand, android_version, sdk_version, display_resolution,
                         battery_level, battery_temperature, is_flagged, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                     ON CONFLICT(serial) DO UPDATE SET
                         hw_serial = excluded.hw_serial,
                         name = excluded.name,
                         device_type = excluded.device_type,
                         address = excluded.address,
                         state = excluded.state,
                         model = excluded.model,
                         brand = excluded.brand,
                         android_version = excluded.android_version,
                         sdk_version = excluded.sdk_version,
                         display_resolution = excluded.display_resolution,
                         battery_level = excluded.battery_level,
                         battery_temperature = excluded.battery_temperature,
                         updated_at = excluded.updated_at",
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
                            row.is_flagged as i32,
                            row.updated_at,
                        ],
                    ),
                    "upsert_device",
                );
            })
            .await;
    }

    pub async fn update_device_state(&self, serial: &str, state: &str) {
        let serial = serial.to_string();
        let state = state.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                let now = now_unix();
                log_exec(
                    conn.execute(
                        "UPDATE a_devices SET state = ?1, updated_at = ?2 WHERE serial = ?3",
                        params![state, now, serial],
                    ),
                    "update_device_state",
                );
            })
            .await;
    }

    pub async fn update_device_props(
        &self,
        serial: &str,
        battery_level: i32,
        battery_temperature: f64,
    ) {
        let serial = serial.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(move |conn| {
            let now = now_unix();
            log_exec(
                conn.execute(
                    "UPDATE a_devices SET battery_level = ?1, battery_temperature = ?2, updated_at = ?3
                     WHERE serial = ?4",
                    params![battery_level, battery_temperature, now, serial],
                ),
                "update_device_props",
            );
        }).await;
    }

    pub async fn mark_offline_except(&self, online_serials: Vec<String>) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(move |conn| {
            let now = now_unix();
            let offline = crate::constants::device_state::OFFLINE;
            if online_serials.is_empty() {
                log_exec(
                    conn.execute(
                        "UPDATE a_devices SET state = ?1, updated_at = ?2 WHERE state != ?1",
                        params![offline, now],
                    ),
                    "mark_all_offline",
                );
            } else {
                let placeholders: Vec<String> =
                    (0..online_serials.len()).map(|i| format!("?{}", i + 3)).collect();
                let sql = format!(
                    "UPDATE a_devices SET state = ?1, updated_at = ?2 WHERE state != ?1 AND serial NOT IN ({})",
                    placeholders.join(",")
                );
                let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
                    Vec::with_capacity(online_serials.len() + 2);
                param_values.push(Box::new(offline.to_string()));
                param_values.push(Box::new(now));
                for s in &online_serials {
                    param_values.push(Box::new(s.clone()));
                }
                let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                    param_values.iter().map(|p| p.as_ref()).collect();
                log_exec(conn.execute(&sql, params_ref.as_slice()), "mark_offline_except");
            }
        }).await;
    }

    pub async fn delete_device(&self, serial: &str) {
        let serial = serial.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute("DELETE FROM a_devices WHERE serial = ?1", params![serial]),
                    "delete_device",
                );
            })
            .await;
    }

    pub async fn flag_device(&self, serial: &str) {
        let serial = serial.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "UPDATE a_devices SET is_flagged = 1 WHERE serial = ?1",
                        params![serial],
                    ),
                    "flag_device",
                );
            })
            .await;
    }

    pub async fn unflag_device(&self, serial: &str) {
        let serial = serial.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "UPDATE a_devices SET is_flagged = 0 WHERE serial = ?1",
                        params![serial],
                    ),
                    "unflag_device",
                );
            })
            .await;
    }

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
                        eprintln!("[db] set_settings_batch 事务开始失败: {}", e);
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
                    eprintln!("[db] set_settings_batch 事务提交失败: {}", e);
                }
            })
            .await;
    }

    /// 批量清理任务（单连接事务内完成）
    pub async fn batch_cleanup_tasks(&self, task_ids: &[String]) {
        let task_ids = task_ids.to_vec();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                let tx = match conn.transaction() {
                    Ok(tx) => tx,
                    Err(e) => {
                        eprintln!("[db] batch_cleanup_tasks 事务开始失败: {}", e);
                        return;
                    },
                };
                for tid in &task_ids {
                    let _ =
                        tx.execute("DELETE FROM a_task_progress WHERE task_id = ?1", params![tid]);
                    let _ = tx.execute("DELETE FROM a_task_runs WHERE task_id = ?1", params![tid]);
                    let _ =
                        tx.execute("DELETE FROM a_task_rounds WHERE task_id = ?1", params![tid]);
                    let _ = tx.execute("DELETE FROM a_task_state WHERE task_id = ?1", params![tid]);
                    let _ = tx.execute("DELETE FROM a_task_defs WHERE task_id = ?1", params![tid]);
                }
                if let Err(e) = tx.commit() {
                    eprintln!("[db] batch_cleanup_tasks 事务提交失败: {}", e);
                }
            })
            .await;
    }

    // ─── 设备读操作 ─────────────────────────────────────────────

    pub async fn device_exists(&self, serial: &str) -> bool {
        let serial = serial.to_string();
        let Ok(conn) = self.pool.get().await else { return false };
        conn.interact(move |conn| {
            conn.query_row("SELECT 1 FROM a_devices WHERE serial = ?1", params![serial], |_| Ok(()))
                .is_ok()
        })
        .await
        .unwrap_or(false)
    }

    pub async fn needs_prop_refresh(&self, serial: &str) -> bool {
        let serial = serial.to_string();
        let Ok(conn) = self.pool.get().await else { return true };
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT model FROM a_devices WHERE serial = ?1",
                params![serial],
                |row| row.get::<_, String>(0),
            )
            .map(|m| m == constants::device_state::UNKNOWN)
            .unwrap_or(true)
        })
        .await
        .unwrap_or(true)
    }

    pub async fn load_all_devices(&self) -> Vec<DeviceRow> {
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(|conn| {
            let mut stmt = conn.prepare(
                "SELECT serial, hw_serial, name, device_type, address, state,
                        model, brand, android_version, sdk_version, display_resolution,
                        battery_level, battery_temperature, is_flagged, updated_at
                 FROM a_devices
                 ORDER BY CASE state WHEN 'Offline' THEN 1 ELSE 0 END, name",
            )?;
            let rows = stmt.query_map([], |row| row_to_device(row))?;
            Ok::<_, rusqlite::Error>(rows.filter_map(|r| r.ok()).collect())
        })
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))
        .unwrap_or_default()
    }

    pub async fn get_device_by_serial(&self, serial: &str) -> Option<DeviceRow> {
        let serial = serial.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT serial, hw_serial, name, device_type, address, state,
                        model, brand, android_version, sdk_version, display_resolution,
                        battery_level, battery_temperature, is_flagged, updated_at
                 FROM a_devices WHERE serial = ?1",
                params![serial],
                |row| row_to_device(row),
            )
            .ok()
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn get_device_by_hw_serial(&self, hw_serial: &str) -> Option<DeviceRow> {
        let hw_serial = hw_serial.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT serial, hw_serial, name, device_type, address, state,
                        model, brand, android_version, sdk_version, display_resolution,
                        battery_level, battery_temperature, is_flagged, updated_at
                 FROM a_devices WHERE hw_serial = ?1",
                params![hw_serial],
                |row| row_to_device(row),
            )
            .ok()
        })
        .await
        .ok()
        .flatten()
    }

    /// 删除全部设备（跨日重置用）
    pub async fn delete_all_devices(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(|conn| {
                let result = conn.execute("DELETE FROM a_devices", []);
                match result {
                    Ok(n) => eprintln!("[db] 跨日重置: 删除了 {} 台设备", n),
                    Err(e) => eprintln!("[db] delete_all_devices 失败: {}", e),
                }
            })
            .await;
    }

    #[allow(dead_code)]
    /// 解除所有设备风控标记（跨日重置用）
    pub async fn unflag_all_devices(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(|conn| {
                log_exec(
                    conn.execute("UPDATE a_devices SET is_flagged = 0 WHERE is_flagged = 1", []),
                    "unflag_all_devices",
                );
            })
            .await;
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

    #[allow(dead_code)]
    pub async fn delete_task_def(&self, task_id: &str) {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute("DELETE FROM a_task_defs WHERE task_id = ?1", params![task_id]),
                    "delete_task_def",
                );
            })
            .await;
    }

    pub async fn get_tasks_by_phone(&self, phone: &str) -> Vec<String> {
        let phone = phone.to_string();
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(move |conn| {
            let mut stmt = conn
                .prepare("SELECT task_id FROM a_task_defs WHERE phone = ?1")
                .unwrap_or_else(|_| conn.prepare("SELECT '' WHERE 0").unwrap());
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

    /// 批量加载全部 city_order（用于 load_tasks 消除 N+1）
    #[allow(dead_code)]
    pub async fn load_all_city_orders(&self) -> std::collections::HashMap<String, Vec<String>> {
        let Ok(conn) = self.pool.get().await else {
            return Default::default();
        };
        conn.interact(|conn| {
            let mut map = std::collections::HashMap::new();
            if let Ok(mut stmt) = conn
                .prepare("SELECT task_id, city_order FROM a_task_defs WHERE city_order IS NOT NULL")
            {
                if let Ok(rows) = stmt
                    .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                {
                    for row in rows.flatten() {
                        if let Ok(order) = serde_json::from_str::<Vec<String>>(&row.1) {
                            map.insert(row.0, order);
                        }
                    }
                }
            }
            map
        })
        .await
        .unwrap_or_default()
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
                             assigned_device=COALESCE(excluded.assigned_device, a_task_state.assigned_device),
                             current_round_id=COALESCE(excluded.current_round_id, a_task_state.current_round_id)",
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
                    "UPDATE a_task_state SET status = 'WAITING', assigned_device = NULL, current_round_id = NULL",
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
            // 分批查询，避免 SQLite 参数上限（默认 999）
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
        let _ = conn.interact(move |conn| {
            let valid_keys: Vec<String> =
                valid_pairs.iter().map(|(c, k)| format!("{}|{}", c, k)).collect();
            let placeholders: Vec<String> =
                (0..valid_keys.len()).map(|i| format!("?{}", i + 2)).collect();
            let sql = format!(
                "DELETE FROM a_task_progress WHERE task_id = ?1 AND (city_name || '|' || keyword_name) NOT IN ({})",
                placeholders.join(",")
            );
            let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
                Vec::with_capacity(valid_keys.len() + 1);
            param_values.push(Box::new(task_id.clone()));
            for key in &valid_keys {
                param_values.push(Box::new(key.clone()));
            }
            let params_ref: Vec<&dyn rusqlite::types::ToSql> =
                param_values.iter().map(|p| p.as_ref()).collect();
            let result = conn.execute(&sql, params_ref.as_slice());
            match result {
                Ok(n) if n > 0 => eprintln!("[db] 清理了 {} 条孤儿进度记录 (task={})", n, task_id),
                Err(e) => eprintln!("[db] cleanup_orphan_progress 失败: {}", e),
                _ => {},
            }
        }).await;
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

    // ─── 执行记录操作 ────────────────────────────────────────────

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

    #[allow(dead_code)]
    pub async fn update_task_run_stats(
        &self,
        task_id: &str,
        started_at: i64,
        cities_done: i32,
        keywords_done: i32,
    ) {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "UPDATE a_task_runs SET cities_done = ?1, keywords_done = ?2
                     WHERE task_id = ?3 AND started_at = ?4",
                        params![cities_done, keywords_done, task_id, started_at],
                    ),
                    "update_task_run_stats",
                );
            })
            .await;
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

fn now_unix() -> i64 {
    crate::constants::now_unix()
}

fn today_str() -> String {
    crate::constants::today_str()
}
