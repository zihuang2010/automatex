/* ===== Shared TypeScript Types ===== */

import { TaskStatus, CityStatus, KeywordStatus } from './constants';

export type TaskStatusType = (typeof TaskStatus)[keyof typeof TaskStatus];
export type CityStatusType = (typeof CityStatus)[keyof typeof CityStatus];
export type KeywordStatusType = (typeof KeywordStatus)[keyof typeof KeywordStatus];

export interface TaskKeyword {
  name: string;
  status: KeywordStatusType;
}

export interface TaskCity {
  name: string;
  poi: string;
  progress: number;
  total: number;
  done: number;
  status: CityStatusType;
  keywords: TaskKeyword[];
}

export interface Task {
  id: string;
  name: string;
  status: TaskStatusType;
  assigned_device: string | null;
  cities: TaskCity[];
}

/** 设备行（与后端 DeviceRow 一一对应） */
export interface DeviceRow {
  serial: string;
  hw_serial: string;
  name: string;
  device_type: string;
  address: string | null;
  state: string;
  model: string;
  brand: string;
  android_version: string;
  sdk_version: string;
  display_resolution: string;
  battery_level: number;
  battery_temperature: number;
  is_flagged: boolean;
  updated_at: number;
}

/** 任务执行统计（与后端 TaskRunStats 对应） */
export interface TaskRunStats {
  last_run_at: number | null;
  today_runs: number;
  today_duration_sec: number;
  today_keywords: number;
}
