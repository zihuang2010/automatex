use std::collections::HashSet;
use std::sync::Arc;

use crate::constants::{run_status, task_status};
use crate::task_provider::Task;

use super::{
    compute_assigned_set, pick_ready_serial, rollback_running_keywords, RunningTask, TaskEngine,
};

impl TaskEngine {
    // ─── 任务操作 ─────────────────────────────────────────────

    /// 启动任务
    pub async fn start_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let devices = self.storage.load_all_devices().await;
        let serial = {
            let mut tasks = self.tasks.write().await;

            let task_status_val = tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.status.clone())
                .ok_or("任务不存在")?;
            if task_status_val != task_status::WAITING {
                return Err("任务状态非 WAITING，无法启动".into());
            }
            let serial = pick_ready_serial(&devices, &tasks)?;

            let task = tasks.iter_mut().find(|t| t.id == task_id).unwrap();
            task.status = task_status::EXECUTING.to_string();
            task.assigned_device = Some(serial.clone());
            serial
        };

        // FIX #7: 创建新轮次 — 失败时回滚状态并返回错误
        let round_id = match self.storage.create_round(task_id).await {
            Some(id) => id,
            None => {
                let mut tasks = self.tasks.write().await;
                if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                    task.status = task_status::WAITING.to_string();
                    task.assigned_device = None;
                }
                return Err("创建轮次失败（数据库错误），无法启动任务".into());
            },
        };

        self.storage
            .save_task_state(task_id, task_status::EXECUTING, Some(&serial), Some(round_id))
            .await;
        let started_at = self.storage.start_task_run(task_id, &serial, round_id).await;

        self.spawn_loop(task_id, started_at, round_id).await;
        self.emit_update().await;
        Ok(())
    }

    /// 暂停任务
    pub async fn pause_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        {
            let tasks = self.tasks.read().await;
            let status = tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.status.clone())
                .ok_or("任务不存在")?;
            if status != task_status::EXECUTING {
                return Err(format!("任务状态为 {}，只有 EXECUTING 可以暂停", status));
            }
        }

        let run_info = {
            let mut running = self.running.write().await;
            if let Some(entry) = running.get(task_id) {
                entry.cancel.cancel();
            }
            running.remove(task_id)
        };

        let round_id = run_info.as_ref().map(|r| r.round_id);
        if let Some(run) = run_info {
            self.storage.finish_task_run(task_id, run.started_at, run_status::PAUSED).await;
        }

        {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                rollback_running_keywords(task);
                task.status = task_status::PAUSED.to_string();
                task.assigned_device = None;
            }
        }

        self.storage.save_task_state(task_id, task_status::PAUSED, None, round_id).await;
        self.emit_update().await;
        Ok(())
    }

    /// 继续任务
    pub async fn resume_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let devices = self.storage.load_all_devices().await;
        let serial = {
            let mut tasks = self.tasks.write().await;

            let task_status_val = tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.status.clone())
                .ok_or("任务不存在")?;
            if task_status_val != task_status::PAUSED && task_status_val != task_status::ERROR {
                return Err(format!("任务状态为 {}，只有 PAUSED/ERROR 可以继续", task_status_val));
            }
            let serial = pick_ready_serial(&devices, &tasks)?;

            let task = tasks.iter_mut().find(|t| t.id == task_id).unwrap();
            rollback_running_keywords(task);
            task.status = task_status::EXECUTING.to_string();
            task.assigned_device = Some(serial.clone());
            serial
        };

        // 继续在当前轮次上执行
        let saved_round_id =
            self.storage.load_task_state(task_id).await.and_then(|(_, _, rid)| rid);

        let round_id = match saved_round_id {
            Some(rid) => rid,
            None => {
                // 无已保存轮次 — 只能创建新轮次（异常恢复路径）
                match self.storage.create_round(task_id).await {
                    Some(id) => id,
                    None => {
                        let mut tasks = self.tasks.write().await;
                        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                            task.status = task_status::PAUSED.to_string();
                            task.assigned_device = None;
                        }
                        return Err("创建轮次失败，无法继续任务".into());
                    },
                }
            },
        };

        self.storage
            .save_task_state(task_id, task_status::EXECUTING, Some(&serial), Some(round_id))
            .await;
        let started_at = self.storage.start_task_run(task_id, &serial, round_id).await;

        self.spawn_loop(task_id, started_at, round_id).await;
        self.emit_update().await;
        Ok(())
    }

    /// 取消正在运行的任务循环，结束 run 记录，清进度，重建任务
    pub(crate) async fn cancel_and_cleanup(&self, task_id: &str) -> Result<Option<Task>, String> {
        {
            let tasks = self.tasks.read().await;
            if !tasks.iter().any(|t| t.id == task_id) {
                return Err("任务不存在".into());
            }
        }

        let run_info = {
            let mut running = self.running.write().await;
            if let Some(entry) = running.get(task_id) {
                entry.cancel.cancel();
            }
            running.remove(task_id)
        };

        if let Some(run) = &run_info {
            self.storage
                .finish_round(run.round_id, crate::constants::round_status::STOPPED)
                .await;
            self.storage.finish_task_run(task_id, run.started_at, run_status::STOPPED).await;
        }
        self.storage.clear_task_progress(task_id).await;
        self.storage.delete_task_state(task_id).await;
        let fresh = crate::task_provider::load_task_by_id(&self.storage, task_id).await;

        Ok(fresh)
    }

    /// 停止任务（清除进度，回到 WAITING）
    pub async fn stop_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let fresh = self.cancel_and_cleanup(task_id).await?;

        if let Some(fresh) = fresh {
            let mut tasks = self.tasks.write().await;
            if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
                tasks[pos] = fresh;
            }
        }

        self.emit_update().await;
        Ok(())
    }

    /// 重试任务（清除进度 + 重新启动）
    pub async fn retry_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let fresh = self.cancel_and_cleanup(task_id).await?;

        let devices = self.storage.load_all_devices().await;
        let serial = {
            let mut tasks = self.tasks.write().await;
            let serial = pick_ready_serial(&devices, &tasks)?;

            if let Some(mut fresh) = fresh {
                fresh.status = task_status::EXECUTING.to_string();
                fresh.assigned_device = Some(serial.clone());
                if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
                    tasks[pos] = fresh;
                }
            } else {
                return Err("任务定义不存在，无法重试".into());
            }
            serial
        };

        // FIX #7: retry 创建新轮次 — 失败时返回错误
        let round_id = match self.storage.create_round(task_id).await {
            Some(id) => id,
            None => {
                return Err("创建轮次失败（数据库错误），无法重试任务".into());
            },
        };

        self.storage
            .save_task_state(task_id, task_status::EXECUTING, Some(&serial), Some(round_id))
            .await;
        let started_at = self.storage.start_task_run(task_id, &serial, round_id).await;

        self.spawn_loop(task_id, started_at, round_id).await;
        self.emit_update().await;
        Ok(())
    }

    /// 释放离线设备上的任务
    pub async fn release_offline_devices(&self, online_serials: &[String]) -> u32 {
        let online_set: HashSet<&str> = online_serials.iter().map(|s| s.as_str()).collect();
        let mut released = 0u32;

        let task_ids_to_release: Vec<String> = {
            let tasks = self.tasks.read().await;
            tasks
                .iter()
                .filter(|t| {
                    t.assigned_device.is_some()
                        && (t.status == task_status::EXECUTING || t.status == task_status::PAUSED)
                        && !online_set.contains(t.assigned_device.as_deref().unwrap_or(""))
                })
                .map(|t| t.id.clone())
                .collect()
        };

        let run_infos: Vec<(String, Option<RunningTask>)> = {
            let mut running = self.running.write().await;
            task_ids_to_release
                .into_iter()
                .map(|tid| {
                    if let Some(entry) = running.get(&tid) {
                        entry.cancel.cancel();
                    }
                    let info = running.remove(&tid);
                    (tid, info)
                })
                .collect()
        };

        if !run_infos.is_empty() {
            for (tid, ri) in &run_infos {
                if let Some(run) = ri {
                    self.storage.finish_task_run(tid, run.started_at, run_status::STOPPED).await;
                }
                let round_id = ri.as_ref().map(|r| r.round_id);
                self.storage.save_task_state(tid, task_status::ERROR, None, round_id).await;
            }
            released = run_infos.len() as u32;
        }

        if released > 0 {
            {
                let mut tasks = self.tasks.write().await;
                for (task_id, _) in &run_infos {
                    if let Some(task) = tasks.iter_mut().find(|t| t.id == *task_id) {
                        task.status = task_status::ERROR.to_string();
                        task.assigned_device = None;
                    }
                }
            }
            self.emit_update().await;
        }
        released
    }

    /// 获取就绪设备列表（在线且未被任务占用）
    /// FIX #13: 使用 HashSet<&str> 避免 String clone，复用 compute_assigned_set
    pub async fn get_ready_serials(&self) -> Vec<String> {
        let devices = self.storage.load_all_devices().await;
        let tasks = self.tasks.read().await;
        let assigned = compute_assigned_set(&tasks);

        devices
            .into_iter()
            .filter(|d| {
                d.state == crate::constants::device_state::DEVICE
                    && !d.is_flagged
                    && !assigned.contains(d.serial.as_str())
            })
            .map(|d| d.serial)
            .collect()
    }
}
