use crate::connection::DeviceEntry;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

/// SQLite 持久化数据库
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// 初始化数据库（创建/打开 + 建表）
    pub fn init(app_data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(app_data_dir).map_err(|e| format!("创建数据目录失败: {}", e))?;

        let db_path = app_data_dir.join("automatex.db");
        let conn = Connection::open(&db_path).map_err(|e| format!("打开数据库失败: {}", e))?;

        // 启用 WAL 模式（提升并发性能）
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| format!("设置 WAL 失败: {}", e))?;

        // 创建设备表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS devices (
                serial      TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                device_type TEXT NOT NULL,
                address     TEXT
            )",
            [],
        )
        .map_err(|e| format!("创建 devices 表失败: {}", e))?;

        // 创建设置表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )
        .map_err(|e| format!("创建 settings 表失败: {}", e))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ─── 设备操作 ────────────────────────────────────────────

    /// 保存单个设备（INSERT OR REPLACE）
    pub fn save_device(&self, entry: &DeviceEntry) {
        let conn = self.conn.lock().unwrap();
        let device_type = match entry.device_type {
            crate::connection::DeviceType::Usb => "usb",
            crate::connection::DeviceType::Wifi => "wifi",
        };
        let _ = conn.execute(
            "INSERT OR REPLACE INTO devices (serial, name, device_type, address)
             VALUES (?1, ?2, ?3, ?4)",
            params![entry.serial, entry.name, device_type, entry.address],
        );
    }

    /// 批量保存设备列表（先清空再插入）
    pub fn save_all_devices(&self, entries: &[DeviceEntry]) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM devices", []);
        for entry in entries {
            let device_type = match entry.device_type {
                crate::connection::DeviceType::Usb => "usb",
                crate::connection::DeviceType::Wifi => "wifi",
            };
            let _ = conn.execute(
                "INSERT OR REPLACE INTO devices (serial, name, device_type, address)
                 VALUES (?1, ?2, ?3, ?4)",
                params![entry.serial, entry.name, device_type, entry.address],
            );
        }
    }

    /// 加载所有设备
    pub fn load_devices(&self) -> Vec<DeviceEntry> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT serial, name, device_type, address FROM devices")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        stmt.query_map([], |row| {
            let serial: String = row.get(0)?;
            let name: String = row.get(1)?;
            let dt: String = row.get(2)?;
            let address: Option<String> = row.get(3)?;
            let device_type = if dt == "usb" {
                crate::connection::DeviceType::Usb
            } else {
                crate::connection::DeviceType::Wifi
            };
            Ok(DeviceEntry {
                serial,
                name,
                device_type,
                address,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// 删除单个设备
    pub fn delete_device(&self, serial: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM devices WHERE serial = ?1", params![serial]);
    }

    /// 清空所有设备
    pub fn clear_devices(&self) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM devices", []);
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
