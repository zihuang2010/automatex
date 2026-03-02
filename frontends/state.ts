import { Task, DeviceRow } from './types';
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

/* ===== Cached Device List (from last refresh) ===== */
let cachedDevices: DeviceRow[] = [];

export function setCachedDevices(devices: DeviceRow[]) {
  cachedDevices = devices;
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

/** 根据 serial 查找设备 */
export function getDeviceBySerial(serial: string): DeviceRow | undefined {
  return cachedDevices.find(d => d.serial === serial);
}
