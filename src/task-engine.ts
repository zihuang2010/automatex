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
            released++;
        }
    }
    return released;
}

/** 启动任务执行引擎（前端定时推进关键词 + 后端持久化） */
export function startTaskExecution(taskId: string) {
    if (taskTimers.has(taskId)) return;

    const timer = setInterval(async () => {
        try {
            const task = globalQueue.find((t: Task) => t.id === taskId);
            if (!task || task.status !== TaskStatus.EXECUTING) {
                clearInterval(timer);
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
                clearInterval(timer);
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
                    clearInterval(timer);
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
    }, 10000);

    taskTimers.set(taskId, timer);
}

/** 停止任务执行定时器 */
export function stopTaskExecution(taskId: string) {
    const timer = taskTimers.get(taskId);
    if (timer) {
        clearInterval(timer);
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
            const readySerials = getReadySerials();
            if (!readySerials.length) {
                showToast('当前没有就绪的设备，请先连接设备');
                return;
            }
            task.assigned_device = readySerials[0];
            task.status = TaskStatus.EXECUTING;
            setSelectedDevice(readySerials[0]);
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
            task.status = TaskStatus.PAUSED;
            task.assigned_device = null;
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
            task.status = TaskStatus.WAITING;
            task.assigned_device = null;
            const startedAt = taskRunStarted.get(taskId);
            if (startedAt) {
                await invoke('finish_task_run', { taskId, startedAt, status: RunStatus.STOPPED });
                taskRunStarted.delete(taskId);
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
            if (freshTask) {
                Object.assign(task, freshTask);
            }
            task.assigned_device = readySerials[0];
            task.status = TaskStatus.EXECUTING;
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

/** 清理所有定时器（页面卸载时调用） */
export function cleanupAllTimers() {
    for (const [id, timer] of taskTimers) {
        clearInterval(timer);
        taskTimers.delete(id);
    }
}
