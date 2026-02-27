use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

/// 数据库中的设备行（包含静态 + 动态属性）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRow {
    pub serial: String,
    pub hw_serial: String,
    pub name: String,
    pub device_type: String, // "usb" | "wifi"
    pub address: Option<String>,
    pub state: String, // "Device" | "Offline"
    pub model: String,
    pub brand: String,
    pub android_version: String,
    pub sdk_version: String,
    pub display_resolution: String,
    pub battery_level: i32,
    pub battery_temperature: f64,
    pub updated_at: i64, // Unix timestamp (秒)
}

/// SQLite 持久化数据库
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// 初始化数据库（创建/打开 + 建表 + 迁移）
    pub fn init(app_data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(app_data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;

        let db_path = app_data_dir.join("automatex.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("打开数据库失败: {}", e))?;

        // 启用 WAL 模式（提升并发性能）
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| format!("设置 WAL 失败: {}", e))?;

        // 创建新的设备表（包含动态属性）
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS devices_v2 (
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

            -- 迁移旧表数据（如果存在）
            INSERT OR IGNORE INTO devices_v2 (serial, name, device_type, address, state)
            SELECT serial, name, device_type, address, 'Offline'
            FROM devices WHERE 1=1;

            -- 创建设置表
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )
        .map_err(|e| format!("建表失败: {}", e))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ─── 设备操作 ────────────────────────────────────────────

    /// 插入或更新设备全量信息
    pub fn upsert_device(&self, row: &DeviceRow) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO devices_v2
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
        );
    }

    /// 仅更新设备状态（上线/下线）
    pub fn update_device_state(&self, serial: &str, state: &str) {
        let conn = self.conn.lock().unwrap();
        let now = now_unix();
        let _ = conn.execute(
            "UPDATE devices_v2 SET state = ?1, updated_at = ?2 WHERE serial = ?3",
            params![state, now, serial],
        );
    }

    /// 仅更新设备动态属性（电量/温度）
    pub fn update_device_props(&self, serial: &str, battery_level: i32, battery_temperature: f64) {
        let conn = self.conn.lock().unwrap();
        let now = now_unix();
        let _ = conn.execute(
            "UPDATE devices_v2 SET battery_level = ?1, battery_temperature = ?2, updated_at = ?3
             WHERE serial = ?4",
            params![battery_level, battery_temperature, now, serial],
        );
    }

    /// 将不在 online_serials 中的设备标记为 Offline
    pub fn mark_offline_except(&self, online_serials: &[&str]) {
        let conn = self.conn.lock().unwrap();
        let now = now_unix();
        if online_serials.is_empty() {
            let _ = conn.execute(
                "UPDATE devices_v2 SET state = 'Offline', updated_at = ?1 WHERE state != 'Offline'",
                params![now],
            );
        } else {
            // SQLite 不支持 IN 绑定数组，用逗号拼接（serial 本身是安全值）
            let placeholders: Vec<String> = online_serials
                .iter()
                .map(|s| format!("'{}'", s.replace('\'', "''")))
                .collect();
            let in_clause = placeholders.join(",");
            let sql = format!(
                "UPDATE devices_v2 SET state = 'Offline', updated_at = {} WHERE state != 'Offline' AND serial NOT IN ({})",
                now, in_clause
            );
            let _ = conn.execute_batch(&sql);
        }
    }

    /// 检查设备是否存在
    pub fn device_exists(&self, serial: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT 1 FROM devices_v2 WHERE serial = ?1",
            params![serial],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// 检查设备是否需要重新获取属性（model 仍为 unknown）
    pub fn needs_prop_refresh(&self, serial: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT model FROM devices_v2 WHERE serial = ?1",
            params![serial],
            |row| row.get::<_, String>(0),
        )
        .map(|m| m == "unknown")
        .unwrap_or(true)
    }

    /// 加载所有设备（供前端读取）
    pub fn load_all_devices(&self) -> Vec<DeviceRow> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT serial, hw_serial, name, device_type, address, state,
                    model, brand, android_version, sdk_version, display_resolution,
                    battery_level, battery_temperature, updated_at
             FROM devices_v2
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

    /// 删除单个设备
    pub fn delete_device(&self, serial: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM devices_v2 WHERE serial = ?1", params![serial]);
    }

    /// 清空所有设备
    pub fn clear_devices(&self) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM devices_v2", []);
    }

    // ─── 设置操作 ────────────────────────────────────────────

    /// 获取设置值
    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .ok()
    }

    /// 保存设置值
    pub fn set_setting(&self, key: &str, value: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        );
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
