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

fn get_schema_version(conn: &Connection) -> i32 {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap_or(0)
}

fn set_schema_version(conn: &Connection, version: i32) {
    let _ = conn.execute_batch(&format!("PRAGMA user_version = {};", version));
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

        // Phase 1: 用裸 Connection 同步执行建表 + 迁移（一次性操作）
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
            CREATE INDEX IF NOT EXISTS idx_runs_sync ON a_task_runs(sync_status);
            CREATE INDEX IF NOT EXISTS idx_runs_task_started ON a_task_runs(task_id, started_at);
            CREATE INDEX IF NOT EXISTS idx_runs_task_date ON a_task_runs(task_id, run_date);
            CREATE INDEX IF NOT EXISTS idx_runs_device_date ON a_task_runs(device_serial, run_date);
            CREATE INDEX IF NOT EXISTS idx_devices_hw_serial ON a_devices(hw_serial);

            CREATE TABLE IF NOT EXISTS a_phone_tasks (
                phone    TEXT NOT NULL,
                task_id  TEXT NOT NULL UNIQUE,
                PRIMARY KEY (phone, task_id)
            );
            CREATE INDEX IF NOT EXISTS idx_phone_tasks_phone ON a_phone_tasks(phone);",
            )
            .map_err(|e| format!("建表失败: {}", e))?;

            // ── 版本化迁移 ──
            let version = get_schema_version(&conn);
            if version < 1 {
                let _ = conn.execute_batch(
                    "ALTER TABLE a_task_cache ADD COLUMN status TEXT NOT NULL DEFAULT 'WAITING';
                     ALTER TABLE a_task_cache ADD COLUMN assigned_device TEXT;",
                );
                set_schema_version(&conn, 1);
            }
            if version < 2 {
                let _ = conn.execute_batch(
                    "ALTER TABLE a_task_runs ADD COLUMN keywords_baseline INTEGER NOT NULL DEFAULT 0;",
                );
                set_schema_version(&conn, 2);
            }
            if version < 3 {
                let _ = conn.execute_batch("ALTER TABLE a_task_cache ADD COLUMN city_order TEXT;");
                set_schema_version(&conn, 3);
            }
            if version < 4 {
                let _ = conn.execute_batch(
                    "ALTER TABLE a_devices ADD COLUMN is_flagged INTEGER NOT NULL DEFAULT 0;",
                );
                set_schema_version(&conn, 4);
            }
            eprintln!("[db] schema_version: {} → 4", version);
            // conn 在此作用域结束时自动关闭
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

    /// 批量设置（单连接内完成，减少池争用）
    pub async fn set_settings_batch(&self, pairs: &[(&str, &str)]) {
        let pairs: Vec<(String, String)> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                for (key, value) in &pairs {
                    log_exec(
                        conn.execute(
                            "INSERT OR REPLACE INTO a_settings (key, value) VALUES (?1, ?2)",
                            params![key, value],
                        ),
                        "set_settings_batch",
                    );
                }
            })
            .await;
    }

    /// 批量清理任务（单连接事务内完成，减少池往返）
    pub async fn batch_cleanup_tasks(&self, task_ids: &[String], phones: &[String]) {
        let task_ids = task_ids.to_vec();
        let phones = phones.to_vec();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(move |conn| {
            let tx = match conn.transaction() {
                Ok(tx) => tx,
                Err(e) => { eprintln!("[db] batch_cleanup_tasks 事务开始失败: {}", e); return; },
            };
            for tid in &task_ids {
                let _ = tx.execute("DELETE FROM a_task_progress WHERE task_id = ?1", params![tid]);
                let _ = tx.execute("UPDATE a_task_cache SET status = 'WAITING', assigned_device = NULL WHERE task_id = ?1", params![tid]);
                let _ = tx.execute("DELETE FROM a_task_cache WHERE task_id = ?1", params![tid]);
            }
            for phone in &phones {
                let _ = tx.execute("DELETE FROM a_phone_tasks WHERE phone = ?1", params![phone]);
            }
            if let Err(e) = tx.commit() {
                eprintln!("[db] batch_cleanup_tasks 事务提交失败: {}", e);
            }
        }).await;
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

    // ─── 手机号任务映射操作 ────────────────────────────────────────

    pub async fn insert_phone_task(&self, phone: &str, task_id: &str) {
        let phone = phone.to_string();
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "INSERT OR REPLACE INTO a_phone_tasks (phone, task_id) VALUES (?1, ?2)",
                        params![phone, task_id],
                    ),
                    "insert_phone_task",
                );
            })
            .await;
    }

    pub async fn get_tasks_by_phone(&self, phone: &str) -> Vec<String> {
        let phone = phone.to_string();
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(move |conn| {
            let mut stmt = conn
                .prepare("SELECT task_id FROM a_phone_tasks WHERE phone = ?1")
                .unwrap_or_else(|_| conn.prepare("SELECT '' WHERE 0").unwrap());
            stmt.query_map(params![phone], |row| row.get::<_, String>(0))
                .ok()
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub async fn get_all_phone_tasks(&self) -> std::collections::HashMap<String, Vec<String>> {
        let Ok(conn) = self.pool.get().await else { return Default::default() };
        conn.interact(|conn| {
            let mut map: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            if let Ok(mut stmt) =
                conn.prepare("SELECT phone, task_id FROM a_phone_tasks ORDER BY phone")
            {
                if let Ok(rows) = stmt
                    .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                {
                    for row in rows.flatten() {
                        map.entry(row.0).or_default().push(row.1);
                    }
                }
            }
            map
        })
        .await
        .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub async fn delete_phone_tasks(&self, phone: &str) {
        let phone = phone.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute("DELETE FROM a_phone_tasks WHERE phone = ?1", params![phone]),
                    "delete_phone_tasks",
                );
            })
            .await;
    }

    #[allow(dead_code)]
    pub async fn clear_all_phone_tasks(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(|conn| {
                log_exec(conn.execute("DELETE FROM a_phone_tasks", []), "clear_all_phone_tasks");
            })
            .await;
    }

    // ─── 任务缓存操作 ────────────────────────────────────────────

    /// 从 DB 读取所有缓存的任务定义
    pub async fn load_all_task_defs(&self) -> Vec<(String, String, String)> {
        // 返回 (task_id, name, payload_json)
        let conn = match self.pool.get().await {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        conn.interact(|conn| {
            let mut stmt = conn.prepare("SELECT task_id, name, payload FROM a_task_cache").unwrap();
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();
            rows
        })
        .await
        .unwrap_or_default()
    }

    /// 从 DB 读取单个任务定义
    pub async fn load_task_def_by_id(&self, task_id: &str) -> Option<(String, String, String)> {
        let task_id = task_id.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT task_id, name, payload FROM a_task_cache WHERE task_id = ?1",
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

    pub async fn upsert_task_cache(&self, task_id: &str, name: &str, payload: &str, version: i64) {
        let task_id = task_id.to_string();
        let name = name.to_string();
        let payload = payload.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(move |conn| {
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
        }).await;
    }

    pub async fn save_task_state(
        &self,
        task_id: &str,
        status: &str,
        assigned_device: Option<&str>,
    ) {
        let task_id = task_id.to_string();
        let status = status.to_string();
        let assigned_device = assigned_device.map(|s| s.to_string());
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                conn.execute(
                    "UPDATE a_task_cache SET status = ?2, assigned_device = ?3 WHERE task_id = ?1",
                    params![task_id, status, assigned_device],
                ),
                "save_task_state",
            );
            })
            .await;
    }

    pub async fn load_task_state(&self, task_id: &str) -> Option<(String, Option<String>)> {
        let task_id = task_id.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT status, assigned_device FROM a_task_cache WHERE task_id = ?1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
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
        let _ = conn.interact(move |conn| {
            log_exec(
                conn.execute(
                    "UPDATE a_task_cache SET status = 'WAITING', assigned_device = NULL WHERE task_id = ?1",
                    params![task_id],
                ),
                "delete_task_state",
            );
        }).await;
    }

    pub async fn save_city_order(&self, task_id: &str, order: &[String]) {
        let task_id = task_id.to_string();
        let json = serde_json::to_string(order).unwrap_or_default();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute(
                        "UPDATE a_task_cache SET city_order = ?2 WHERE task_id = ?1",
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
                    "SELECT city_order FROM a_task_cache WHERE task_id = ?1",
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

    #[allow(dead_code)]
    pub async fn load_task_cache(&self, task_id: &str) -> Option<TaskCacheRow> {
        let task_id = task_id.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT task_id, name, payload, version, fetched_at FROM a_task_cache WHERE task_id = ?1",
                params![task_id],
                |row| {
                    Ok(TaskCacheRow {
                        task_id: row.get(0)?, name: row.get(1)?, payload: row.get(2)?,
                        version: row.get(3)?, fetched_at: row.get(4)?,
                    })
                },
            ).ok()
        }).await.ok().flatten()
    }

    #[allow(dead_code)]
    pub async fn load_all_task_caches(&self) -> Vec<TaskCacheRow> {
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(|conn| {
            let mut stmt = conn.prepare(
                "SELECT task_id, name, payload, version, fetched_at FROM a_task_cache ORDER BY name",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(TaskCacheRow {
                    task_id: row.get(0)?, name: row.get(1)?, payload: row.get(2)?,
                    version: row.get(3)?, fetched_at: row.get(4)?,
                })
            })?;
            Ok::<_, rusqlite::Error>(rows.filter_map(|r| r.ok()).collect())
        }).await.unwrap_or_else(|_| Ok(Vec::new())).unwrap_or_default()
    }

    #[allow(dead_code)]
    pub async fn delete_task_cache(&self, task_id: &str) {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                log_exec(
                    conn.execute("DELETE FROM a_task_cache WHERE task_id = ?1", params![task_id]),
                    "delete_task_cache",
                );
            })
            .await;
    }

    // ─── 执行进度操作 ────────────────────────────────────────────

    pub async fn record_keyword_done(
        &self,
        task_id: &str,
        city_name: &str,
        keyword_name: &str,
        device_serial: &str,
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
                        (task_id, city_name, keyword_name, status, completed_at, device_serial, sync_status)
                     VALUES (?1, ?2, ?3, 'ok', ?4, ?5, 'pending')",
                    params![task_id, city_name, keyword_name, now, device_serial],
                ),
                "record_keyword_done",
            );
        }).await;
    }

    pub async fn load_task_progress(&self, task_id: &str) -> Vec<ProgressRow> {
        let task_id = task_id.to_string();
        let Ok(conn) = self.pool.get().await else { return Vec::new() };
        conn.interact(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT task_id, city_name, keyword_name, status, completed_at, device_serial
                 FROM a_task_progress WHERE task_id = ?1",
            )?;
            let rows = stmt.query_map(params![task_id], |row| {
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

    pub async fn start_task_run(&self, task_id: &str, device_serial: &str) -> i64 {
        let task_id = task_id.to_string();
        let device_serial = device_serial.to_string();
        let Ok(conn) = self.pool.get().await else { return now_unix() };
        conn.interact(move |conn| {
            let now = now_unix();
            let today = today_str();
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
                         keywords_done = (SELECT COUNT(*) FROM a_task_progress WHERE task_id = ?4) - keywords_baseline,
                         cities_done = (SELECT COUNT(DISTINCT city_name) FROM a_task_progress WHERE task_id = ?4)
                     WHERE task_id = ?4 AND started_at = ?5",
                    params![now, duration, status, task_id, started_at],
                ),
                "finish_task_run",
            );
        }).await;
    }

    pub async fn cleanup_stale_assignments(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(|conn| {
            let affected = conn.execute(
                "UPDATE a_task_cache SET status = 'PAUSED', assigned_device = NULL WHERE status = 'EXECUTING'",
                [],
            );
            match affected {
                Ok(n) if n > 0 => eprintln!("[db] 清理了 {} 条残留 EXECUTING 任务", n),
                Err(e) => eprintln!("[db] cleanup_stale_assignments 失败: {}", e),
                _ => {},
            }
        }).await;
    }

    pub async fn cleanup_orphan_runs(&self) {
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn.interact(|conn| {
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
    crate::constants::now_unix()
}

fn today_str() -> String {
    crate::constants::today_str()
}
