/* ===== Shared TypeScript Types ===== */
import { CityStatus, KeywordStatus, TaskPresentationStatus, TaskStatus } from './constants';

export type TaskStatusType = (typeof TaskStatus)[keyof typeof TaskStatus];
export type TaskPresentationStatusType =
  (typeof TaskPresentationStatus)[keyof typeof TaskPresentationStatus];
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
  runtime_status?: string | null;
  presentation_status?: TaskPresentationStatusType | null;
  assigned_device: string | null;
  cities: TaskCity[];
  interval_minute?: number | null;
  round_no?: number;
  current_city_name?: string | null;
  current_keyword_name?: string | null;
  next_round_at?: number | null;
}

export interface TaskSummary {
  id: string;
  name: string;
  status: TaskStatusType;
  runtime_status: string | null;
  presentation_status: TaskPresentationStatusType;
  assigned_device: string | null;
  city_count: number;
  keyword_total: number;
  keyword_done: number;
  progress: number;
  // active_city_* 字段已移除：前端使用 current_city_name 和 cities 数组，这 4 个字段从未被消费
  current_city_name: string | null;
  current_keyword_name: string | null;
  interval_minute: number | null;
  round_no: number;
  next_round_at: number | null;
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
