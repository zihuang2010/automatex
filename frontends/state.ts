import { Task } from './types';
import { TaskStatus } from './constants';

export let selectedDevice: string | null = null;
export let globalQueue: Task[] = [];
export let activeTask: Task | null = null;
export let activeCityIdx = 0;

/* ===== State Mutation Functions ===== */

export function setSelectedDevice(serial: string | null) {
  selectedDevice = serial;
}

export function setGlobalQueue(tasks: Task[]) {
  globalQueue = tasks;
}

export function setActiveTask(task: Task | null) {
  activeTask = task;
}

export function setActiveCityIdx(idx: number) {
  activeCityIdx = idx;
}

/* ===== Derived State (单一来源，避免重复实现) ===== */

/** 获取正在执行任务的已分配设备 serial 集合 */
export function getAssignedDeviceSerials(): Set<string> {
  return new Set(
    globalQueue
      .filter(t => t.assigned_device && t.status === TaskStatus.EXECUTING)
      .map(t => t.assigned_device!),
  );
}
