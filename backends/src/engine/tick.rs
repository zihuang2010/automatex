use std::sync::Arc;
use std::time::Duration;

use rand::RngExt;
use tauri::Emitter;
use tokio_util::sync::CancellationToken;

use crate::constants::{city_status, keyword_status, round_status, run_status, task_status};

use super::{rollback_running_keywords, RunningTask, TaskEngine};

/// tick 产生的副作用，在释放 tasks 写锁后执行
pub(crate) enum TickEffect {
    /// 推进了一个关键词（记录的是真正完成 RUN→OK 的那个）
    KeywordDone {
        task_id: String,
        city_name: String,
        kw_name: String,
        device_serial: String,
    },
    /// 整个任务执行完成（可能包含最后一个完成的关键词）
    TaskSuccess {
        task_id: String,
        last_keyword: Option<(String, String, String)>, // (city, kw, device)
    },
    /// 设备离线，任务需要标记 ERROR
    DeviceOffline {
        task_id: String,
        device_serial: String,
    },
    /// 风控触发：标记设备 + 任务 ERROR
    RiskControl {
        task_id: String,
        device_serial: String,
    },
    /// 无需任何 DB 操作（城市切换、初始 RUN 标记等）
    None,
}

impl TaskEngine {
    // ─── 内部方法 ─────────────────────────────────────────────

    pub(crate) async fn spawn_loop(
        self: &Arc<Self>,
        task_id: &str,
        started_at: i64,
        round_id: i64,
    ) {
        let cancel = CancellationToken::new();
        self.running.write().await.insert(
            task_id.to_string(),
            RunningTask { cancel: cancel.clone(), started_at, round_id },
        );

        let engine = Arc::clone(self);
        let tid = task_id.to_string();

        tokio::spawn(async move {
            let completed = loop {
                tokio::select! {
                    _ = cancel.cancelled() => break false,
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {
                        let done = engine.tick(&tid).await;
                        engine.emit_update().await;
                        if done { break true; }
                    }
                }
            };

            let run_info = engine.running.write().await.remove(&tid);
            if completed {
                if let Some(ref run) = run_info {
                    engine.storage.finish_round(run.round_id, round_status::COMPLETED).await;
                }
                let sa = run_info.map(|r| r.started_at).unwrap_or(started_at);
                engine.storage.finish_task_run(&tid, sa, run_status::COMPLETED).await;
            }
        });
    }

    /// 核心 tick 逻辑
    pub(crate) async fn tick(&self, task_id: &str) -> bool {
        // Step 1: DB 查询在写锁外执行
        let device_serial_for_check = {
            let tasks = self.tasks.read().await;
            tasks.iter().find(|t| t.id == task_id).and_then(|t| t.assigned_device.clone())
        };
        let device_online = match device_serial_for_check.as_deref() {
            Some(serial) => self
                .storage
                .get_device_by_serial(serial)
                .await
                .map(|d| d.state == crate::constants::device_state::DEVICE)
                .unwrap_or(false),
            None => false,
        };

        // Step 2: 获取写锁执行状态变更
        let effect = 'effect: {
            let mut tasks = self.tasks.write().await;
            let task = match tasks.iter_mut().find(|t| t.id == task_id) {
                Some(t) => t,
                None => break 'effect TickEffect::None,
            };

            if task.status != task_status::EXECUTING {
                break 'effect TickEffect::None;
            }

            let device_serial = task.assigned_device.clone().unwrap_or_default();

            if !device_online {
                rollback_running_keywords(task);
                task.status = task_status::ERROR.to_string();
                task.assigned_device = None;
                break 'effect TickEffect::DeviceOffline {
                    task_id: task_id.to_string(),
                    device_serial,
                };
            }

            // ── 风控模拟（可配置开关） ────────────────────────
            if crate::constants::debug::MOCK_RISK_ENABLED {
                let mut rng = rand::rng();
                if rng.random_bool(crate::constants::debug::MOCK_RISK_PROBABILITY) {
                    eprintln!(
                        "[engine] risk-control triggered (simulated): task={}, device={}",
                        task_id, device_serial
                    );
                    rollback_running_keywords(task);
                    task.status = task_status::ERROR.to_string();
                    task.assigned_device = None;
                    break 'effect TickEffect::RiskControl {
                        task_id: task_id.to_string(),
                        device_serial,
                    };
                }
            }

            // 找活跃城市或激活第一个 pending
            let active_idx =
                task.cities.iter().position(|c| c.status == city_status::ACTIVE).or_else(|| {
                    let idx = task.cities.iter().position(|c| c.status == city_status::PENDING)?;
                    task.cities[idx].status = city_status::ACTIVE.to_string();
                    Some(idx)
                });

            let Some(active_idx) = active_idx else {
                task.status = task_status::SUCCESS.to_string();
                task.assigned_device = None;
                break 'effect TickEffect::TaskSuccess {
                    task_id: task_id.to_string(),
                    last_keyword: None,
                };
            };

            let city = &mut task.cities[active_idx];

            // FIX #5: 先记录真正完成的关键词（RUN → OK），再标记下一个
            let mut completed_kw: Option<(String, String)> = None;
            for kw in city.keywords.iter_mut() {
                if kw.status == keyword_status::RUN {
                    completed_kw = Some((city.name.clone(), kw.name.clone()));
                    kw.status = keyword_status::OK.to_string();
                    city.done += 1;
                    city.progress = if city.total > 0 {
                        ((city.done as f64 / city.total as f64) * 100.0).round() as i32
                    } else {
                        0
                    };
                }
            }

            // 找下一个 pending 关键词
            let next_idx = city.keywords.iter().position(|k| k.status == keyword_status::PENDING);

            if let Some(idx) = next_idx {
                city.keywords[idx].status = keyword_status::RUN.to_string();

                if let Some((done_city, done_kw)) = completed_kw {
                    TickEffect::KeywordDone {
                        task_id: task_id.to_string(),
                        city_name: done_city,
                        kw_name: done_kw,
                        device_serial,
                    }
                } else {
                    TickEffect::None
                }
            } else if let Some((done_city, done_kw)) = completed_kw {
                city.status = city_status::DONE.to_string();
                city.progress = 100;

                let has_next = task
                    .cities
                    .iter_mut()
                    .find(|c| c.status == city_status::PENDING)
                    .map(|nc| {
                        nc.status = city_status::ACTIVE.to_string();
                    })
                    .is_some();

                if has_next {
                    TickEffect::KeywordDone {
                        task_id: task_id.to_string(),
                        city_name: done_city,
                        kw_name: done_kw,
                        device_serial,
                    }
                } else {
                    task.status = task_status::SUCCESS.to_string();
                    task.assigned_device = None;
                    TickEffect::TaskSuccess {
                        task_id: task_id.to_string(),
                        last_keyword: Some((done_city, done_kw, device_serial)),
                    }
                }
            } else {
                city.status = city_status::DONE.to_string();
                city.progress = 100;
                let next = task.cities.iter_mut().find(|c| c.status == city_status::PENDING);
                if let Some(nc) = next {
                    nc.status = city_status::ACTIVE.to_string();
                    TickEffect::None
                } else {
                    task.status = task_status::SUCCESS.to_string();
                    task.assigned_device = None;
                    TickEffect::TaskSuccess { task_id: task_id.to_string(), last_keyword: None }
                }
            }
        }; // ← 写锁释放

        // Step 3: 锁外执行 DB 操作
        match effect {
            TickEffect::KeywordDone { task_id, city_name, kw_name, device_serial } => {
                let round_id =
                    self.running.read().await.get(&task_id).map(|r| r.round_id).unwrap_or(0);
                self.storage
                    .record_keyword_done(&task_id, &city_name, &kw_name, &device_serial, round_id)
                    .await;
                false
            },
            TickEffect::TaskSuccess { task_id, last_keyword } => {
                if let Some((city, kw, device)) = last_keyword {
                    let round_id =
                        self.running.read().await.get(&task_id).map(|r| r.round_id).unwrap_or(0);
                    self.storage.record_keyword_done(&task_id, &city, &kw, &device, round_id).await;
                }
                if let Some(run) = self.running.read().await.get(&task_id) {
                    self.storage.finish_round(run.round_id, round_status::COMPLETED).await;
                }
                self.storage.save_task_state(&task_id, task_status::SUCCESS, None, None).await;
                true
            },
            TickEffect::DeviceOffline { task_id, device_serial } => {
                let still_offline = self
                    .storage
                    .get_device_by_serial(&device_serial)
                    .await
                    .map(|d| d.state != crate::constants::device_state::DEVICE)
                    .unwrap_or(true);
                if !still_offline {
                    let mut tasks = self.tasks.write().await;
                    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                        task.status = task_status::EXECUTING.to_string();
                        task.assigned_device = Some(device_serial.clone());
                    }
                    let round_id = self.running.read().await.get(&task_id).map(|r| r.round_id);
                    self.storage
                        .save_task_state(
                            &task_id,
                            task_status::EXECUTING,
                            Some(&device_serial),
                            round_id,
                        )
                        .await;
                    eprintln!("[engine] 设备已恢复在线，回滚任务状态: {}", task_id);
                    return false;
                }
                self.storage.save_task_state(&task_id, task_status::ERROR, None, None).await;
                true
            },
            TickEffect::RiskControl { task_id, device_serial } => {
                self.storage.save_task_state(&task_id, task_status::ERROR, None, None).await;
                self.storage.flag_device(&device_serial).await;
                let _ = self.app_handle.emit(
                    crate::constants::tauri_event::RISK_CONTROL,
                    serde_json::json!({
                        "task_id": task_id,
                        "device_serial": device_serial,
                        "message": "设备风控触发，任务已停止，设备已标记"
                    }),
                );
                let _ = self.app_handle.emit(crate::constants::tauri_event::DEVICES_CHANGED, ());
                true
            },
            TickEffect::None => false,
        }
    }
}
