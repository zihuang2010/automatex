use std::collections::HashSet;
use std::sync::Arc;

use tauri::Emitter;

use crate::constants::task_status;
use crate::task_provider;

use super::TaskEngine;

impl TaskEngine {
    // ─── MQTT 消息处理 ─────────────────────────────────────────

    /// 处理 MQTT 踢设备指令
    pub async fn handle_device_kick(self: &Arc<Self>, hw_serials: Vec<String>) -> u32 {
        let mut kicked = 0u32;

        for hw_serial in &hw_serials {
            let device = self.storage.get_device_by_hw_serial(hw_serial).await;
            let Some(device) = device else { continue };
            let serial = device.serial.clone();

            let task_to_pause: Option<String> = {
                let tasks = self.tasks.read().await;
                tasks
                    .iter()
                    .find(|t| {
                        t.assigned_device.as_deref() == Some(&serial)
                            && (t.status == task_status::EXECUTING
                                || t.status == task_status::PAUSED)
                    })
                    .map(|t| t.id.clone())
            };

            if let Some(task_id) = task_to_pause {
                if let Err(e) = self.pause_task(&task_id).await {
                    eprintln!("[engine] 踢设备时暂停任务失败: task={}, err={}", task_id, e);
                }
            }

            self.storage.delete_device(&serial).await;
            eprintln!("[engine] 设备已踢下线: hw_serial={}, serial={}", hw_serial, serial);
            kicked += 1;
        }

        if kicked > 0 {
            let _ = self.app_handle.emit(crate::constants::tauri_event::DEVICES_CHANGED, ());
            self.emit_update().await;
        }
        kicked
    }

    /// 增量合并单个任务（reload_task 核心逻辑）
    async fn merge_single_task(&self, task_id: &str) {
        let new_def = match self.http.fetch_task(task_id).await {
            Ok(def) => def,
            Err(e) => {
                eprintln!("[engine] merge_single_task: 获取任务定义失败 {}: {}", task_id, e);
                return;
            },
        };

        let payload = match serde_json::to_string(&new_def.cities) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[engine] merge_single_task: 序列化 payload 失败: {}", e);
                return;
            },
        };

        self.storage.upsert_task_def(task_id, &new_def.name, &payload, 1, "").await;

        let was_success = {
            let tasks = self.tasks.read().await;
            tasks.iter().any(|t| t.id == task_id && t.status == task_status::SUCCESS)
        };
        if was_success {
            self.storage.clear_task_progress(task_id).await;
            self.storage.delete_task_state(task_id).await;
            eprintln!("[engine] 任务 {} 原状态 SUCCESS，已清空旧进度", task_id);
        }

        let merged = task_provider::build_task(&self.storage, new_def).await;

        let mut tasks = self.tasks.write().await;
        if let Some(local) = tasks.iter_mut().find(|t| t.id == task_id) {
            let is_running = local.status == task_status::EXECUTING
                || local.status == task_status::PAUSED
                || local.status == task_status::ERROR;

            if is_running {
                let saved_status = local.status.clone();
                let saved_device = local.assigned_device.clone();
                // 保留当前 cities/keywords 的运行时进度状态
                let old_cities = std::mem::take(&mut local.cities);
                *local = merged;
                local.status = saved_status;
                local.assigned_device = saved_device;
                // 将旧的进度状态合并回来
                for city in &mut local.cities {
                    if let Some(old_city) = old_cities.iter().find(|c| c.name == city.name) {
                        city.status = old_city.status.clone();
                        city.done = old_city.done;
                        city.progress = old_city.progress;
                        for kw in &mut city.keywords {
                            if let Some(old_kw) =
                                old_city.keywords.iter().find(|k| k.name == kw.name)
                            {
                                kw.status = old_kw.status.clone();
                            }
                        }
                    }
                }
                eprintln!("[engine] merge_single_task: 任务 {} 执行中，增量合并完成", task_id);
            } else {
                *local = merged;
                eprintln!("[engine] merge_single_task: 任务 {} 未执行，全量覆盖", task_id);
            }
        } else {
            eprintln!("[engine] merge_single_task: 新任务 {} 已插入", task_id);
            tasks.push(merged);
        }

        let valid_pairs: Vec<(String, String)> = tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| {
                t.cities
                    .iter()
                    .flat_map(|c| c.keywords.iter().map(move |k| (c.name.clone(), k.name.clone())))
                    .collect()
            })
            .unwrap_or_default();
        drop(tasks);

        self.storage.cleanup_orphan_progress(task_id, valid_pairs).await;
    }

    /// 处理 MQTT 任务数据变更通知
    pub async fn handle_task_reload(self: &Arc<Self>, action: &str, task_id: Option<&str>) {
        match action {
            "reload_all" => {
                eprintln!("[engine] 收到 reload_all，重新加载所有任务");
                self.reload_tasks().await;
                self.emit_update().await;
            },
            "reload_task" => {
                if let Some(tid) = task_id {
                    {
                        let mut guard = self.reloading.lock().await;
                        if guard.contains(tid) {
                            eprintln!("[engine] reload_task 防重入跳过: {}", tid);
                            return;
                        }
                        guard.insert(tid.to_string());
                    }
                    eprintln!("[engine] 收到 reload_task: {}", tid);
                    self.merge_single_task(tid).await;
                    self.emit_update().await;
                    self.reloading.lock().await.remove(tid);
                }
            },
            "delete_task" => {
                if let Some(tid) = task_id {
                    eprintln!("[engine] 收到 delete_task: {}", tid);
                    let is_running = {
                        let tasks = self.tasks.read().await;
                        tasks.iter().any(|t| {
                            t.id == tid
                                && (t.status == task_status::EXECUTING
                                    || t.status == task_status::PAUSED
                                    || t.status == task_status::ERROR)
                        })
                    };
                    if is_running {
                        let _ = self.stop_task(tid).await;
                    }
                    {
                        let mut tasks = self.tasks.write().await;
                        tasks.retain(|t| t.id != tid);
                    }
                    self.storage.batch_cleanup_tasks(&[tid.to_string()]).await;
                    self.emit_update().await;
                }
            },
            _ => {
                eprintln!("[engine] 未知的 task reload action: {}", action);
            },
        }
    }

    /// 处理手机号解绑/被抢占 — 停止运行、释放设备、从内存和 DB 彻底清除
    pub async fn handle_phones_unbind(self: &Arc<Self>, phones: Vec<String>) -> u32 {
        let mut all_task_ids: Vec<String> = Vec::new();
        for phone in &phones {
            let task_ids = self.storage.get_tasks_by_phone(phone).await;
            eprintln!("[engine] 解绑手机号 {}: 关联 {} 个任务", phone, task_ids.len());
            all_task_ids.extend(task_ids);
        }
        all_task_ids.sort();
        all_task_ids.dedup();

        if all_task_ids.is_empty() {
            return 0;
        }

        let all_ids_set: HashSet<String> = all_task_ids.iter().cloned().collect();

        // ── 1. 取消正在运行的任务循环 + 释放设备 + 结束 run ──
        {
            let mut running = self.running.write().await;
            for task_id in &all_task_ids {
                if let Some(entry) = running.get(task_id) {
                    entry.cancel.cancel();
                    eprintln!("[engine] 取消运行中任务: {}", task_id);
                }
                if let Some(run) = running.remove(task_id) {
                    // 正确结束 run 记录
                    let storage = Arc::clone(&self.storage);
                    let tid = task_id.clone();
                    tokio::spawn(async move {
                        storage
                            .finish_task_run(
                                &tid,
                                run.started_at,
                                crate::constants::run_status::STOPPED,
                            )
                            .await;
                    });
                }
            }
        }

        // ── 2. 从内存任务列表移除 + 释放 assigned_device ──
        {
            let mut tasks = self.tasks.write().await;
            tasks.retain(|t| !all_ids_set.contains(&t.id));
        }

        self.storage.batch_cleanup_tasks(&all_task_ids).await;

        let removed = all_task_ids.len() as u32;
        eprintln!(
            "[engine] 批量清理完成: {} 个任务（解绑手机号: {:?}）",
            all_task_ids.len(),
            phones
        );

        removed
    }
}
