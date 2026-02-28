import { invoke } from '@tauri-apps/api/core';
import { TaskStatus, CityStatus, KeywordStatus, RunStatus } from './constants';
import { Task, TaskCity, TaskKeyword } from './types';
import {
    globalQueue,
    taskTimers,
    taskRunStarted,
    setSelectedDevice,
    getReadySerials,
} from './state';
import { showToast } from './utils';

// 回调注册（由 main.ts 初始化后设置，避免循环依赖）
let _onRefresh: (() => Promise<void>) | null = null;

export function setRefreshCallbacks(onRefresh: () => Promise<void>) {
    _onRefresh = onRefresh;
}

/* ===== Task Execution Engine ===== */

/** 释放离线设备上的任务 */
export function releaseTasksForOfflineDevices(onlineSerials: Set<string>): number {
    let released = 0;
    for (const task of globalQueue) {
        if (
            task.assigned_device &&
            (task.status === TaskStatus.EXECUTING || task.status === TaskStatus.PAUSED) &&
            !onlineSerials.has(task.assigned_device)
        ) {
            console.warn(
                `[TaskEngine] 设备 ${task.assigned_device} 离线,释放任务 "${task.name}" (${task.id})`,
            );
            stopTaskExecution(task.id);
            task.status = TaskStatus.ERROR;
            task.assigned_device = null; // #7: 清除已离线的设备绑定
            // #1: 持久化 ERROR 状态
            invoke('save_task_state', {
                taskId: task.id,
                status: TaskStatus.ERROR,
                assignedDevice: null,
            }).catch(() => {});
            released++;
        }
    }
    return released;
}

/** 启动任务执行引擎（前端定时推进关键词 + 后端持久化）
 *  #6: 改用递归 setTimeout，等 async 完成后再启动下一轮，避免并行 IPC */
export function startTaskExecution(taskId: string) {
    if (taskTimers.has(taskId)) return;

    async function tick() {
        try {
            const task = globalQueue.find((t: Task) => t.id === taskId);
            if (!task || task.status !== TaskStatus.EXECUTING) {
                taskTimers.delete(taskId);
                return;
            }

            let activeCity = task.cities.find((c: TaskCity) => c.status === CityStatus.ACTIVE);
            if (!activeCity) {
                activeCity = task.cities.find((c: TaskCity) => c.status === CityStatus.PENDING);
                if (activeCity) activeCity.status = CityStatus.ACTIVE;
            }
            if (!activeCity) {
                task.status = TaskStatus.SUCCESS;
                taskTimers.delete(taskId);
                const startedAt = taskRunStarted.get(taskId);
                if (startedAt) {
                    await invoke('finish_task_run', {
                        taskId,
                        startedAt,
                        status: RunStatus.COMPLETED,
                    });
                    taskRunStarted.delete(taskId);
                }
                tickRefresh();
                return;
            }

            const nextKw = activeCity.keywords.find(
                (k: TaskKeyword) => k.status === KeywordStatus.PENDING,
            );
            if (nextKw) {
                activeCity.keywords.forEach((k: TaskKeyword) => {
                    if (k.status === KeywordStatus.RUN) k.status = KeywordStatus.OK;
                });
                nextKw.status = KeywordStatus.RUN;
                activeCity.done++;
                activeCity.progress = Math.round((activeCity.done / activeCity.total) * 100);
                await invoke('record_keyword_complete', {
                    taskId: task.id,
                    cityName: activeCity.name,
                    keywordName: nextKw.name,
                    deviceSerial: task.assigned_device ?? '',
                });
            } else {
                activeCity.keywords.forEach((k: TaskKeyword) => {
                    if (k.status === KeywordStatus.RUN) k.status = KeywordStatus.OK;
                });
                activeCity.status = CityStatus.DONE;
                activeCity.progress = 100;

                const nextCity = task.cities.find((c: TaskCity) => c.status === CityStatus.PENDING);
                if (nextCity) {
                    nextCity.status = CityStatus.ACTIVE;
                } else {
                    task.status = TaskStatus.SUCCESS;
                    taskTimers.delete(taskId);
                    const startedAt = taskRunStarted.get(taskId);
                    if (startedAt) {
                        await invoke('finish_task_run', {
                            taskId,
                            startedAt,
                            status: RunStatus.COMPLETED,
                        });
                        taskRunStarted.delete(taskId);
                    }
                }
            }

            tickRefresh();
        } catch (err) {
            console.error(`[TaskEngine] 任务 ${taskId} 执行异常:`, err);
        }

        // 递归 setTimeout：上一轮完成后才启动下一轮计时
        if (taskTimers.has(taskId)) {
            const next = setTimeout(tick, 10000);
            taskTimers.set(taskId, next);
        }
    }

    const timer = setTimeout(tick, 10000);
    taskTimers.set(taskId, timer);
}

/** 停止任务执行定时器 */
export function stopTaskExecution(taskId: string) {
    const timer = taskTimers.get(taskId);
    if (timer) {
        clearTimeout(timer);
        taskTimers.delete(taskId);
    }
}

/** 执行引擎的 UI 刷新回调 */
function tickRefresh() {
    _onRefresh?.();
}

/** 任务操作后统一刷新 */
export async function afterTaskAction() {
    await _onRefresh?.();
}

/** 注册全局任务操作回调 */
export function registerTaskActions() {
    const actions: Record<string, (taskId: string) => Promise<void>> = {
        __taskStart: async (taskId: string) => {
            const task = globalQueue.find(t => t.id === taskId);
            if (!task) return;
            if (task.status !== TaskStatus.WAITING) return; // #10: 防重复启动
            const readySerials = getReadySerials();
            if (!readySerials.length) {
                showToast('当前没有就绪的设备，请先连接设备');
                return;
            }
            task.assigned_device = readySerials[0];
            task.status = TaskStatus.EXECUTING;
            setSelectedDevice(readySerials[0]);
            // #1: 持久化状态
            await invoke('save_task_state', {
                taskId,
                status: TaskStatus.EXECUTING,
                assignedDevice: readySerials[0],
            });
            const startedAt = await invoke<number>('start_task_run', {
                taskId,
                deviceSerial: readySerials[0],
            });
            taskRunStarted.set(taskId, startedAt);
            startTaskExecution(taskId);
            await afterTaskAction();
        },

        __taskPause: async (taskId: string) => {
            const task = globalQueue.find(t => t.id === taskId);
            if (!task) return;
            stopTaskExecution(taskId);
            task.status = TaskStatus.PAUSED; // #2: 先设状态
            task.assigned_device = null; // 再清绑定
            // #1: 持久化状态
            await invoke('save_task_state', {
                taskId,
                status: TaskStatus.PAUSED,
                assignedDevice: null,
            });
            const startedAt = taskRunStarted.get(taskId);
            if (startedAt) {
                await invoke('finish_task_run', { taskId, startedAt, status: RunStatus.PAUSED });
                taskRunStarted.delete(taskId);
            }
            setSelectedDevice(null);
            await afterTaskAction();
        },

        __taskResume: async (taskId: string) => {
            const task = globalQueue.find(t => t.id === taskId);
            if (!task) return;
            const readySerials = getReadySerials();
            if (!readySerials.length) {
                showToast('没有可用设备');
                return;
            }
            const serial = readySerials[0];
            task.assigned_device = serial;
            task.status = TaskStatus.EXECUTING;
            setSelectedDevice(serial);
            // #1: 持久化状态
            await invoke('save_task_state', {
                taskId,
                status: TaskStatus.EXECUTING,
                assignedDevice: serial,
            });
            const startedAt = await invoke<number>('start_task_run', {
                taskId,
                deviceSerial: serial,
            });
            taskRunStarted.set(taskId, startedAt);
            startTaskExecution(taskId);
            await afterTaskAction();
        },

        __taskStop: async (taskId: string) => {
            const task = globalQueue.find(t => t.id === taskId);
            if (!task) return;
            stopTaskExecution(taskId);
            const startedAt = taskRunStarted.get(taskId);
            if (startedAt) {
                await invoke('finish_task_run', { taskId, startedAt, status: RunStatus.STOPPED });
                taskRunStarted.delete(taskId);
            }
            // 停止 = 彻底回到 WAITING + 清除进度
            await invoke('clear_task_progress', { taskId });
            // 从后端重新加载干净的任务数据
            const freshTasks = await invoke<Task[]>('list_tasks');
            const freshTask = freshTasks.find((t: Task) => t.id === taskId);
            const idx = globalQueue.indexOf(task);
            if (freshTask && idx >= 0) {
                globalQueue[idx] = freshTask;
            } else {
                task.status = TaskStatus.WAITING;
                task.assigned_device = null;
                task.cities.forEach(c => {
                    c.done = 0;
                    c.progress = 0;
                    c.status = 'pending';
                    c.keywords.forEach(k => {
                        k.status = 'pending';
                    });
                });
            }
            setSelectedDevice(null);
            await afterTaskAction();
        },

        __taskRetry: async (taskId: string) => {
            const task = globalQueue.find(t => t.id === taskId);
            if (!task) return;
            const readySerials = getReadySerials();
            if (!readySerials.length) {
                showToast('没有可用设备');
                return;
            }
            await invoke('clear_task_progress', { taskId });
            const freshTasks = await invoke<Task[]>('list_tasks');
            const freshTask = freshTasks.find((t: Task) => t.id === taskId);
            // #3: 整体替换而非 Object.assign，避免旧属性残留
            const idx = globalQueue.indexOf(task);
            const updatedTask: Task = {
                ...(freshTask ?? task),
                assigned_device: readySerials[0],
                status: TaskStatus.EXECUTING,
            };
            if (idx >= 0) globalQueue[idx] = updatedTask;
            setSelectedDevice(readySerials[0]);
            const startedAt = await invoke<number>('start_task_run', {
                taskId,
                deviceSerial: readySerials[0],
            });
            taskRunStarted.set(taskId, startedAt);
            startTaskExecution(taskId);
            await afterTaskAction();
        },
    };

    // 注册到 window（供 HTML onclick 调用）
    for (const [name, fn] of Object.entries(actions)) {
        (window as unknown as Record<string, unknown>)[name] = fn;
    }
}

/** 清理所有定时器（页面卸载时调用）
 *  #14: 同时结束未完成的执行记录，避免 DB 中残留 running 状态 */
export function cleanupAllTimers() {
    for (const [id, timer] of taskTimers) {
        clearTimeout(timer);
        taskTimers.delete(id);
    }
    // 尝试结束所有未关闭的执行记录
    for (const [taskId, startedAt] of taskRunStarted) {
        invoke('finish_task_run', { taskId, startedAt, status: RunStatus.STOPPED }).catch(() => {});
    }
    taskRunStarted.clear();
}
