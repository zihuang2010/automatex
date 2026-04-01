import { invoke } from '@tauri-apps/api/core';
import { type UnlistenFn, listen } from '@tauri-apps/api/event';

import {
  activeTask,
  globalQueue,
  setActiveCityIdx,
  setActiveTask,
  setActiveTaskDetail,
  setGlobalQueue,
} from './state';
import { TaskSummary } from './types';
import { showToast } from './utils';

// 回调注册（由 main.ts 初始化后设置，避免循环依赖）
let _onRefresh: (() => Promise<void>) | null = null;
let _taskUpdateUnlisten: UnlistenFn | null = null;

export function setRefreshCallbacks(onRefresh: () => Promise<void>) {
  _onRefresh = onRefresh;
}

function syncActiveTaskFromQueue() {
  if (activeTask) {
    const currentTask = activeTask;
    const updated = globalQueue.find(task => task.id === currentTask.id);
    if (updated) {
      setActiveTask(updated);
      return;
    }
    setActiveTask(null);
    setActiveTaskDetail(null);
    setActiveCityIdx(0);
  }

  if (globalQueue.length > 0) {
    setActiveTask(globalQueue[0]);
    setActiveTaskDetail(null);
    setActiveCityIdx(0);
  }
}

/* ===== 后端引擎事件监听 ===== */

/** 初始化后端引擎事件监听 + 加载初始任务 */
export async function initEngine() {
  // 监听后端推送的任务状态更新
  if (!_taskUpdateUnlisten) {
    _taskUpdateUnlisten = await listen<{ tasks: TaskSummary[] }>('task://update', event => {
      const { tasks } = event.payload;
      setGlobalQueue(tasks);
      syncActiveTaskFromQueue();
      _onRefresh?.();
    });
  }

  // 从后端引擎加载初始任务列表
  const tasks = await invoke<TaskSummary[]>('engine_get_tasks');
  setGlobalQueue(tasks);
  syncActiveTaskFromQueue();
  await _onRefresh?.();
  return tasks;
}

/* ===== 任务操作（调用后端引擎）===== */

export function registerTaskActions() {
  const actions: Record<string, (taskId: string) => Promise<void>> = {
    __taskStart: async (taskId: string) => {
      try {
        await invoke('engine_start_task', { taskId });
      } catch (e) {
        showToast(String(e));
      }
    },

    __taskPause: async (taskId: string) => {
      try {
        await invoke('engine_pause_task', { taskId });
      } catch (e) {
        showToast(String(e));
      }
    },

    __taskResume: async (taskId: string) => {
      try {
        await invoke('engine_resume_task', { taskId });
      } catch (e) {
        showToast(String(e));
      }
    },

    __taskStop: async (taskId: string) => {
      try {
        await invoke('engine_stop_task', { taskId });
      } catch (e) {
        showToast(String(e));
      }
    },

    __taskRetry: async (taskId: string) => {
      try {
        await invoke('engine_retry_task', { taskId });
      } catch (e) {
        showToast(String(e));
      }
    },
  };

  // 注册到 window（供 HTML onclick 调用）
  for (const [name, fn] of Object.entries(actions)) {
    (window as unknown as Record<string, unknown>)[name] = fn;
  }
}
