use super::{log_exec, now_unix, row_to_device, Database, DeviceRow};
use rusqlite::params;

impl Database {
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
            let rows = stmt.query_map([], row_to_device)?;
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
                row_to_device,
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
                row_to_device,
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
}
