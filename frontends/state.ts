import { TaskStatus } from './constants';
import { Task, TaskSummary } from './types';

export let selectedDevice: string | null = null;
export let globalQueue: TaskSummary[] = [];
export let activeTask: TaskSummary | null = null;
export let activeTaskDetail: Task | null = null;
export let activeCityIdx = 0;
const TASK_ORDER_STORAGE_KEY = 'automatex.task_order';

function readTaskOrder(): string[] {
  try {
    const raw = localStorage.getItem(TASK_ORDER_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === 'string') : [];
  } catch {
    return [];
  }
}

function writeTaskOrder(order: string[]) {
  try {
    localStorage.setItem(TASK_ORDER_STORAGE_KEY, JSON.stringify(order));
  } catch {
    /* ignore localStorage failures */
  }
}

function applyTaskOrder(tasks: TaskSummary[]): TaskSummary[] {
  if (tasks.length <= 1) {
    writeTaskOrder(tasks.map(task => task.id));
    return [...tasks];
  }

  const savedOrder = readTaskOrder();
  const taskIds = tasks.map(task => task.id);
  const normalizedOrder = [
    ...savedOrder.filter(id => taskIds.includes(id)),
    ...taskIds.filter(id => !savedOrder.includes(id)),
  ];

  const orderMap = new Map(normalizedOrder.map((id, index) => [id, index]));
  const orderedTasks = [...tasks].sort(
    (a, b) =>
      (orderMap.get(a.id) ?? Number.MAX_SAFE_INTEGER) -
      (orderMap.get(b.id) ?? Number.MAX_SAFE_INTEGER),
  );

  writeTaskOrder(orderedTasks.map(task => task.id));
  return orderedTasks;
}

/* ===== State Mutation Functions ===== */

export function setSelectedDevice(serial: string | null) {
  selectedDevice = serial;
}

export function setGlobalQueue(tasks: TaskSummary[]) {
  globalQueue = applyTaskOrder(tasks);
}

export function setActiveTask(task: TaskSummary | null) {
  activeTask = task;
}

export function setActiveTaskDetail(task: Task | null) {
  activeTaskDetail = task;
}

export function setActiveCityIdx(idx: number) {
  activeCityIdx = idx;
}

export function reorderGlobalQueue(newOrder: string[]) {
  if (globalQueue.length === 0) return;
  const orderedIds = [
    ...newOrder.filter(id => globalQueue.some(task => task.id === id)),
    ...globalQueue.map(task => task.id).filter(id => !newOrder.includes(id)),
  ];
  writeTaskOrder(orderedIds);
  globalQueue = applyTaskOrder(globalQueue);
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
