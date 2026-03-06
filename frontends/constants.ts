// 状态字面量统一管理（与后端 constants.rs 保持一致）

export const TaskStatus = {
  WAITING: 'waiting',
  EXECUTING: 'executing',
  PAUSED: 'paused',
  SUCCESS: 'success',
  ERROR: 'error',
} as const;

export const CityStatus = {
  PENDING: 'pending',
  ACTIVE: 'active',
  DONE: 'done',
} as const;

export const KeywordStatus = {
  PENDING: 'pending',
  RUN: 'run',
  OK: 'ok',
} as const;

export const DeviceState = {
  OFFLINE: 'Offline',
  DEVICE: 'Device',
} as const;
