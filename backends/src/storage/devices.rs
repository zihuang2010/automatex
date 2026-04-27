use super::{log_exec, now_unix, row_to_device, Database, DeviceRow};
use rusqlite::params;
use tracing::{debug, error, info};

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
                // Fix-SQL2b：使用 params_from_iter 替代 Box<dyn ToSql> 逐个堆分配
                let placeholders: Vec<String> =
                    (0..online_serials.len()).map(|i| format!("?{}", i + 3)).collect();
                let sql = format!(
                    "UPDATE a_devices SET state = ?1, updated_at = ?2 WHERE state != ?1 AND serial NOT IN ({})",
                    placeholders.join(",")
                );
                let all_params: Vec<Box<dyn rusqlite::types::ToSql>> = {
                    let mut v: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::with_capacity(online_serials.len() + 2);
                    v.push(Box::new(offline.to_string()));
                    v.push(Box::new(now));
                    for s in &online_serials {
                        v.push(Box::new(s.clone()));
                    }
                    v
                };
                log_exec(
                    conn.execute(&sql, rusqlite::params_from_iter(all_params.iter().map(|p| p.as_ref()))),
                    "mark_offline_except",
                );
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

    // ─── 设备端口分配 ──────────────────────────────────────────

    /// 为设备分配（或返回已分配的）PC 本地端口。
    ///
    /// ## 策略
    /// - **幂等**：若该 serial 已有 `local_port`，直接返回，不重新分配。
    /// - **递增分配**：从 `PORT_BASE`（7899）起，在已分配的最大端口上 +1。
    /// - **原子操作**：在单个 `interact` 闭包内完成查询 + 写入，避免并发竞争。
    ///
    /// ## 返回值
    /// 分配或已持有的端口号。若 DB 操作失败，返回 `PORT_BASE`（降级保证不崩溃）。
    pub async fn assign_device_port(&self, serial: &str) -> u16 {
        let serial = serial.to_string();
        let port_base = crate::constants::phone_client::PORT_BASE;

        let Ok(conn) = self.pool.get().await else {
            return port_base;
        };

        conn.interact(move |conn| -> u16 {
            // Fix-L3：使用 IMMEDIATE 事务包裹读-改-写操作，
            // 避免并发连接的 TOCTOU 竞争导致端口分配重复
            let tx = match conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
                Ok(tx) => tx,
                Err(e) => {
                    error!(op = "assign_device_port", error = %e, "事务开始失败");
                    return port_base;
                },
            };

            // ── 1. 检查是否已分配 ──
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT local_port FROM a_devices WHERE serial = ?1 AND local_port IS NOT NULL",
                    rusqlite::params![serial],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .unwrap_or(None);

            if let Some(port) = existing {
                // 已分配，无需写入，直接回滚事务释放锁
                let _ = tx.finish();
                return port.clamp(port_base as i64, u16::MAX as i64) as u16;
            }

            // ── 2. 分配新端口（max 已分配 + 1，最小为 PORT_BASE）──
            let max_existing: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(local_port), ?1 - 1) FROM a_devices WHERE local_port IS NOT NULL",
                    rusqlite::params![port_base as i64],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(port_base as i64 - 1);

            let new_port = ((max_existing + 1).max(port_base as i64)) as u16;

            // ── 3. 写回 DB ──
            if let Err(e) = tx.execute(
                "UPDATE a_devices SET local_port = ?1 WHERE serial = ?2",
                rusqlite::params![new_port as i64, serial],
            ) {
                error!(op = "assign_device_port", serial = %serial, port = new_port, error = %e, "设备端口写入失败");
                return port_base;
            }

            if let Err(e) = tx.commit() {
                error!(op = "assign_device_port", error = %e, "事务提交失败");
                return port_base;
            }

            debug!(serial = %serial, port = new_port, "设备分配本地端口");
            new_port
        })
        .await
        .unwrap_or(port_base)
    }

    /// 查询设备已分配的本地端口（未分配返回 `None`）
    #[allow(dead_code)]
    pub async fn get_device_port(&self, serial: &str) -> Option<u16> {
        let serial = serial.to_string();
        let conn = self.pool.get().await.ok()?;
        conn.interact(move |conn| {
            conn.query_row(
                "SELECT local_port FROM a_devices WHERE serial = ?1 AND local_port IS NOT NULL",
                rusqlite::params![serial],
                |row| row.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
            .map(|p| p.clamp(1, u16::MAX as i64) as u16)
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
                    Ok(n) => info!(count = n, "跨日重置: 删除设备"),
                    Err(e) => error!(op = "delete_all_devices", error = %e, "数据库操作失败"),
                }
            })
            .await;
    }

    /// 合并同一物理设备（hw_serial 相同）的不同 ADB transport serial 行。
    ///
    /// 一台设备先 USB 接入（serial = hw_serial）后又切到无线（serial = IP:port），
    /// ADB `track_devices` 把它当成两个 transport 上报，monitor 各 upsert 一条
    /// `a_devices` 行 —— hw_serial 相同、serial 不同，UI 就出现重复设备卡。
    /// 本方法在 IMMEDIATE 事务内做四件事：
    /// 1. 找出 hw_serial 相同但 serial != new_serial 的所有旧行；
    /// 2. 把 `a_task_state.assigned_device`、`a_task_runs.device_serial`、
    ///    `a_task_progress.device_serial` 三个引用列里的旧 serial 改写为 new_serial；
    /// 3. 把旧行 local_port 继承到新行（仅当新行为空时）；
    /// 4. 删除旧 a_devices 行。
    ///
    /// 仅在 monitor 拿到真实 hw_serial（≠ serial 占位）后调用，
    /// 否则会把 placeholder 行错误合并。
    pub async fn reconcile_device_by_hw_serial(&self, new_serial: &str, hw_serial: &str) {
        if hw_serial.trim().is_empty() {
            return;
        }
        let new_serial = new_serial.to_string();
        let hw_serial = hw_serial.to_string();
        let Ok(conn) = self.pool.get().await else { return };
        let _ = conn
            .interact(move |conn| {
                let tx = match conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                {
                    Ok(tx) => tx,
                    Err(e) => {
                        error!(op = "reconcile_device", error = %e, "事务开始失败");
                        return;
                    },
                };

                let old_rows: Vec<(String, Option<i64>)> = {
                    let mut stmt = match tx.prepare(
                        "SELECT serial, local_port FROM a_devices
                         WHERE hw_serial = ?1 AND serial != ?2",
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            error!(op = "reconcile_device", error = %e, "查询旧行失败");
                            return;
                        },
                    };
                    let mapped = match stmt.query_map(params![hw_serial, new_serial], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
                    }) {
                        Ok(rows) => rows,
                        Err(e) => {
                            error!(op = "reconcile_device", error = %e, "迭代旧行失败");
                            return;
                        },
                    };
                    let collected: Vec<(String, Option<i64>)> =
                        mapped.filter_map(|r| r.ok()).collect();
                    collected
                };

                if old_rows.is_empty() {
                    return;
                }

                let inherited_port: Option<i64> = old_rows.iter().find_map(|(_, p)| *p);

                for (old_serial, _) in &old_rows {
                    log_exec(
                        tx.execute(
                            "UPDATE a_task_state SET assigned_device = ?1 WHERE assigned_device = ?2",
                            params![new_serial, old_serial],
                        ),
                        "reconcile.task_state",
                    );
                    log_exec(
                        tx.execute(
                            "UPDATE a_task_runs SET device_serial = ?1 WHERE device_serial = ?2",
                            params![new_serial, old_serial],
                        ),
                        "reconcile.task_runs",
                    );
                    log_exec(
                        tx.execute(
                            "UPDATE a_task_progress SET device_serial = ?1 WHERE device_serial = ?2",
                            params![new_serial, old_serial],
                        ),
                        "reconcile.task_progress",
                    );
                    log_exec(
                        tx.execute(
                            "DELETE FROM a_devices WHERE serial = ?1",
                            params![old_serial],
                        ),
                        "reconcile.delete_old",
                    );
                }

                if let Some(port) = inherited_port {
                    log_exec(
                        tx.execute(
                            "UPDATE a_devices
                             SET local_port = COALESCE(local_port, ?1)
                             WHERE serial = ?2",
                            params![port, new_serial],
                        ),
                        "reconcile.inherit_port",
                    );
                }

                if let Err(e) = tx.commit() {
                    error!(op = "reconcile_device", error = %e, "事务提交失败");
                } else {
                    info!(
                        hw_serial = %hw_serial,
                        new_serial = %new_serial,
                        merged = old_rows.len(),
                        "合并设备行（hw_serial 去重）"
                    );
                }
            })
            .await;
    }
}
