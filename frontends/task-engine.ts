import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import { activeTask, globalQueue, setActiveTask, setGlobalQueue } from './state';
import { Task } from './types';
import { showToast } from './utils';

// 回调注册（由 main.ts 初始化后设置，避免循环依赖）
let _onRefresh: (() => Promise<void>) | null = null;

export function setRefreshCallbacks(onRefresh: () => Promise<void>) {
  _onRefresh = onRefresh;
}

/* ===== 后端引擎事件监听 ===== */

/** 初始化后端引擎事件监听 + 加载初始任务 */
export async function initEngine() {
  // 监听后端推送的任务状态更新
  await listen<{ tasks: Task[] }>('task://update', event => {
    const { tasks } = event.payload;
    // 更新 globalQueue（保持引用）
    setGlobalQueue(tasks);

    // 更新 activeTask 引用（指向新的 queue 中的对应任务）
    if (activeTask) {
      const currentId = activeTask.id;
      const updated = globalQueue.find(t => t.id === currentId);
      if (updated) {
        setActiveTask(updated);
      } else {
        // 当前任务已被删除，自动选中队列中的第一个任务
        setActiveTask(globalQueue.length > 0 ? globalQueue[0] : null);
      }
    } else if (globalQueue.length > 0) {
      // 之前没有选中任务但现在有了，自动选中第一个
      setActiveTask(globalQueue[0]);
    }

    // 触发 UI 刷新
    _onRefresh?.();
  });

  // 从后端引擎加载初始任务列表
  const tasks = await invoke<Task[]>('engine_get_tasks');
  setGlobalQueue(tasks);
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
