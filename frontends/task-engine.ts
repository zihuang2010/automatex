import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import { rerenderDeviceCardsFromCache } from './devices';
import { loadChainForDevice } from './queue';
import {
  activeTask,
  globalQueue,
  setActiveCityIdx,
  setActiveTask,
  setActiveTaskDetail,
  setGlobalQueue,
} from './state';
import { renderTaskView } from './task-view';
import { TaskSummary } from './types';
import { showToast } from './utils';

// LOGIC-9 修复：_taskUpdateUnlisten 已移至 main.ts 的 appUnlisteners 统一管理

function syncActiveTaskFromQueue() {
  if (activeTask) {
    const currentTask = activeTask;
    const updated = globalQueue.find(task => task.id === currentTask.id);
    if (updated) {
      const summaryChanged =
        updated.status !== currentTask.status ||
        updated.runtime_status !== currentTask.runtime_status ||
        updated.presentation_status !== currentTask.presentation_status ||
        updated.assigned_device !== currentTask.assigned_device ||
        updated.progress !== currentTask.progress ||
        updated.keyword_done !== currentTask.keyword_done ||
        updated.current_city_name !== currentTask.current_city_name ||
        updated.current_keyword_name !== currentTask.current_keyword_name ||
        updated.next_round_at !== currentTask.next_round_at ||
        updated.round_no !== currentTask.round_no;
      if (summaryChanged) {
        setActiveTaskDetail(null);
      }
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

async function renderTaskStateViews() {
  syncActiveTaskFromQueue();
  loadChainForDevice('');
  rerenderDeviceCardsFromCache();
  await renderTaskView();
}

/* ===== 后端引擎事件监听 ===== */

/**
 * 初始化后端引擎事件监听 + 加载初始任务
 * 返回 unlisten 函数，由调用方（main.ts）将其纳入 appUnlisteners 统一管理
 */
export async function initEngine(): Promise<{ tasks: TaskSummary[]; unlisten: () => void }> {
  // 监听后端推送的任务状态更新
  const unlisten = await listen<{ tasks: TaskSummary[] }>('task://update', async event => {
    const { tasks } = event.payload;
    setGlobalQueue(tasks);
    await renderTaskStateViews();
  });

  // 从后端引擎加载初始任务列表
  const tasks = await invoke<TaskSummary[]>('engine_get_tasks');
  setGlobalQueue(tasks);
  await renderTaskStateViews();
  return { tasks, unlisten };
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
