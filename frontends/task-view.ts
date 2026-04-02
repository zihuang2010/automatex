import { invoke } from '@tauri-apps/api/core';
/** 绑定城市卡片拖拽事件（SortableJS） */
import Sortable from 'sortablejs';

import { CityStatus, KeywordStatus, TaskPresentationStatus } from './constants';
import {
  activeCityIdx,
  activeTask,
  activeTaskDetail,
  globalQueue,
  setActiveCityIdx,
  setActiveTask,
  setActiveTaskDetail,
  setSelectedDevice,
} from './state';
import { Task, TaskCity, TaskKeyword, TaskRunStats } from './types';
import { $, esc, formatRunTime, getCountdownState, getPresentationState } from './utils';

// 回调注册（由 main.ts 初始化后设置，避免循环依赖）
let _onUpdateCardSelection: (() => void) | null = null;
let _onLoadChainForDevice: ((serial: string) => void) | null = null;

export function setTaskViewCallbacks(
  onUpdateCardSelection: () => void,
  onLoadChainForDevice: (serial: string) => void,
) {
  _onUpdateCardSelection = onUpdateCardSelection;
  _onLoadChainForDevice = onLoadChainForDevice;
}

/* ===== 分区更新：缓存上一次各区域的 HTML ===== */
let _prevTaskId: string | null = null;
let _prevHeaderHtml = '';
let _prevMetricsHtml = '';
let _prevCityHtml = '';
let _prevKwHtml = '';
let _prevKwInfoHtml = '';

// TaskRunStats 缓存（避免每次渲染都跨进程查 DB）
let _statsCache: { taskId: string; stats: TaskRunStats; taskStatus: string } | null = null;

export async function loadTasksForDevice(serial: string) {
  const task = globalQueue.find(t => t.assigned_device === serial);
  if (task && task !== activeTask) {
    setActiveTask(task);
    setActiveTaskDetail(null);
    setActiveCityIdx(0);
  }
  await renderTaskView();
  if (serial) {
    _onLoadChainForDevice?.(serial);
  }
}

function detailMatchesSummary(task: Task, summary: typeof activeTask): boolean {
  if (!summary) return false;
  if (task.id !== summary.id) return false;
  if (task.status !== summary.status) return false;
  if ((task.runtime_status ?? null) !== (summary.runtime_status ?? null)) return false;
  if ((task.presentation_status ?? null) !== (summary.presentation_status ?? null)) return false;
  if ((task.assigned_device ?? null) !== summary.assigned_device) return false;
  if ((task.current_city_name ?? null) !== (summary.current_city_name ?? null)) return false;
  if ((task.current_keyword_name ?? null) !== (summary.current_keyword_name ?? null)) return false;
  if ((task.next_round_at ?? null) !== (summary.next_round_at ?? null)) return false;
  if ((task.round_no ?? 0) !== summary.round_no) return false;
  const total = task.cities.reduce((sum, city) => sum + city.total, 0);
  const done = task.cities.reduce((sum, city) => sum + city.done, 0);
  if (total !== summary.keyword_total || done !== summary.keyword_done) return false;
  const progress = total > 0 ? Math.round((done / total) * 100) : 0;
  if (progress !== summary.progress) return false;
  return true;
}

async function ensureActiveTaskDetail(): Promise<Task | null> {
  if (!activeTask) {
    setActiveTaskDetail(null);
    return null;
  }

  if (activeTaskDetail && detailMatchesSummary(activeTaskDetail, activeTask)) {
    return activeTaskDetail;
  }

  const detail = await invoke<Task | null>('engine_get_task_detail', { taskId: activeTask.id });
  setActiveTaskDetail(detail);
  return detail;
}

/* ===== 骨架：首次渲染时创建带 id 的容器结构 ===== */
function ensureSkeleton(mid: HTMLElement): boolean {
  if (mid.querySelector('#tv-header')) return false; // 已存在
  mid.innerHTML = `
    <div id="tv-header" class="shrink-0"></div>
    <div id="tv-metrics" class="shrink-0"></div>
    <div class="flex-1 bg-white rounded-xl border border-s200 shadow-sm overflow-hidden flex flex-col min-h-0">
      <div class="px-4 pt-3 pb-0 shrink-0 border-b border-s100">
        <div id="tv-city-header" class="flex items-center justify-between mb-2.5"></div>
        <div id="tv-cities" class="flex gap-2 overflow-x-auto pb-3 scrollbar-hide"></div>
      </div>
      <div class="flex-1 overflow-y-auto p-4 min-h-0">
        <div id="tv-kw-info" class="flex items-center justify-between mb-3"></div>
        <div id="tv-kw-grid" class="flex flex-wrap gap-2"></div>
      </div>
    </div>`;
  // 首次创建，清空缓存
  _prevHeaderHtml = '';
  _prevMetricsHtml = '';
  _prevCityHtml = '';
  _prevKwHtml = '';
  _prevKwInfoHtml = '';
  return true;
}

/* ===== 高效更新：只更新变化的区域 ===== */
function patchHtml(id: string, html: string, prev: string): string {
  if (html !== prev) {
    const el = document.getElementById(id);
    if (el) el.innerHTML = html;
  }
  return html;
}

/* ===== 主渲染函数 ===== */
export async function renderTaskView() {
  const mid = $('#col-mid')!;
  const task = await ensureActiveTaskDetail();

  if (!task || !activeTask) {
    mid.innerHTML =
      '<div class="empty-hint" style="padding:40px;text-align:center">选择任务以查看详情</div>';
    _prevTaskId = null;
    return;
  }

  const city = task.cities[activeCityIdx] ?? task.cities[0];
  if (!city) {
    mid.innerHTML = '<div class="empty-hint">无城市数据</div>';
    _prevTaskId = null;
    return;
  }

  // 任务切换时重建骨架
  if (_prevTaskId !== task.id) {
    _prevTaskId = task.id;
    ensureSkeleton(mid);
  } else if (!mid.querySelector('#tv-header')) {
    ensureSkeleton(mid);
  }

  // ── Header ──
  const headerHtml = buildHeader(task, activeTask);
  _prevHeaderHtml = patchHtml('tv-header', headerHtml, _prevHeaderHtml);

  // ── Metrics ──（异步获取统计信息）
  const metricsHtml = await buildMetrics(task);
  _prevMetricsHtml = patchHtml('tv-metrics', metricsHtml, _prevMetricsHtml);

  // ── City Header ──
  const citiesDone = task.cities.filter((c: TaskCity) => c.status === CityStatus.DONE).length;
  const cityHeaderHtml = `
    <div class="flex items-center gap-2">
      <span class="material-symbols-outlined icon-sm text-blue-400">location_city</span>
      <span class="text-[11px] font-black text-s500 uppercase tracking-tight">覆盖城市</span>
    </div>
    <span class="text-[11px] font-bold text-s400">${citiesDone}/${task.cities.length} 已完成</span>`;
  // city header 直接内联更新，不做 prev 缓存（轻量）
  const cityHeaderEl = document.getElementById('tv-city-header');
  if (cityHeaderEl) cityHeaderEl.innerHTML = cityHeaderHtml;

  // ── City Cards ──
  const cityCardsHtml = buildCityCards(task);
  _prevCityHtml = patchHtml('tv-cities', cityCardsHtml, _prevCityHtml);
  bindCityDragEvents(task.id);

  // ── Keywords Info Bar ──
  const kwInfoHtml = `
    <div class="flex items-center gap-1.5">
      <span class="material-symbols-outlined icon-sm text-blue-400 fill-1">sell</span>
      <span class="text-[11px] font-black text-s500 uppercase tracking-tight">关键词</span>
      <span class="px-1.5 py-0.5 bg-s100 text-s500 rounded text-[10px] font-bold">${city.done}/${city.total}</span>
    </div>
    <div class="relative">
      <input class="w-40 pl-9 pr-2 py-1 bg-s50 border border-s200 rounded text-[11px] focus:ring-1 focus:ring-blue-200 focus:border-blue-400 focus:bg-white outline-none transition-all placeholder:text-s300" placeholder="搜索..." type="text" id="kw-filter-input" oninput="window.__filterKw(this.value)" />
      <span class="material-symbols-outlined absolute left-3 top-1/2 -translate-y-1/2 text-s300 text-xs">search</span>
    </div>`;

  // 保留搜索框的值和焦点
  const existingInput = document.getElementById('kw-filter-input') as HTMLInputElement | null;
  const savedSearch = existingInput?.value || '';
  const hadFocus = document.activeElement === existingInput;
  _prevKwInfoHtml = patchHtml('tv-kw-info', kwInfoHtml, _prevKwInfoHtml);
  if (savedSearch) {
    const newInput = document.getElementById('kw-filter-input') as HTMLInputElement | null;
    if (newInput) {
      newInput.value = savedSearch;
      if (hadFocus) newInput.focus();
    }
  }

  // ── Keyword Grid ──
  const kwGridHtml = buildKeywordGrid(city);
  _prevKwHtml = patchHtml('tv-kw-grid', kwGridHtml, _prevKwHtml);

  // 恢复搜索过滤状态
  if (savedSearch) {
    const filterFn = (window as unknown as Record<string, (v: string) => void>).__filterKw;
    if (filterFn) filterFn(savedSearch);
  }
}

/* ===== 各区域构建函数 ===== */

function buildHeader(
  task: {
    id: string;
    name: string;
    status: string;
    runtime_status?: string | null;
    assigned_device: string | null;
  },
  summary: typeof activeTask,
): string {
  type BadgeInfo = { bg: string; text: string; border: string; label: string };
  const presentation = getPresentationState(task);
  const intervalWaiting = presentation === TaskPresentationStatus.WAITING_NEXT_ROUND;
  const intervalPaused = presentation === TaskPresentationStatus.PAUSED_WAITING;
  const badgeMap: Record<string, BadgeInfo> = {
    [TaskPresentationStatus.READY]: {
      bg: 'bg-gray-100',
      text: 'text-gray-600',
      border: 'border-gray-200',
      label: '待启动',
    },
    [TaskPresentationStatus.RUNNING]: {
      bg: 'bg-blue-100',
      text: 'text-blue-700',
      border: 'border-blue-200',
      label: '执行中',
    },
    [TaskPresentationStatus.PAUSED_MANUAL]: {
      bg: 'bg-amber-100',
      text: 'text-amber-700',
      border: 'border-amber-200',
      label: '已暂停',
    },
    [TaskPresentationStatus.COMPLETED]: {
      bg: 'bg-green-100',
      text: 'text-green-700',
      border: 'border-green-200',
      label: '已完成',
    },
    [TaskPresentationStatus.ERROR_PAUSED]: {
      bg: 'bg-red-100',
      text: 'text-red-700',
      border: 'border-red-200',
      label: '异常暂停',
    },
  };
  const badge = intervalWaiting
    ? {
        bg: 'bg-violet-100',
        text: 'text-violet-700',
        border: 'border-violet-200',
        label: '等待中',
      }
    : intervalPaused
      ? {
          bg: 'bg-amber-100',
          text: 'text-amber-700',
          border: 'border-amber-200',
          label: '等待暂停',
        }
      : badgeMap[presentation] || badgeMap[TaskPresentationStatus.READY];
  const deviceTag = task.assigned_device
    ? `<p class="text-[11px] text-s400 font-semibold mt-1.5 h-4 leading-4"><span class="material-symbols-outlined text-sm icon-xs align-middle mr-0.5">smartphone</span>${esc(task.assigned_device)}</p>`
    : `<p class="h-4 mt-1.5"></p>`;

  const btnStart =
    presentation === TaskPresentationStatus.READY
      ? `<button onclick="window.__taskStart('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-emerald-500 hover:bg-emerald-600 text-white font-medium rounded-xl transition-all shadow-lg shadow-emerald-500/20 text-xs"><span class="material-symbols-outlined text-base">play_arrow</span><span class="font-bold" style="letter-spacing:0.15em">启动</span></button>`
      : '';
  const btnPause =
    presentation === TaskPresentationStatus.RUNNING ||
    presentation === TaskPresentationStatus.WAITING_NEXT_ROUND
      ? `<button onclick="window.__taskPause('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-amber-500 hover:bg-amber-600 text-white font-medium rounded-xl transition-all shadow-lg shadow-amber-500/20 text-xs"><span class="material-symbols-outlined text-base">pause</span><span class="font-bold" style="letter-spacing:0.15em">暂停</span></button>`
      : '';
  const btnResume =
    presentation === TaskPresentationStatus.PAUSED_MANUAL ||
    presentation === TaskPresentationStatus.PAUSED_WAITING ||
    presentation === TaskPresentationStatus.ERROR_PAUSED
      ? `<button onclick="window.__taskResume('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-emerald-500 hover:bg-emerald-600 text-white font-medium rounded-xl transition-all shadow-lg shadow-emerald-500/20 text-xs"><span class="material-symbols-outlined text-base">play_arrow</span><span class="font-bold" style="letter-spacing:0.15em">继续</span></button>`
      : '';
  const btnRetry =
    presentation === TaskPresentationStatus.ERROR_PAUSED ||
    presentation === TaskPresentationStatus.COMPLETED ||
    presentation === TaskPresentationStatus.PAUSED_MANUAL ||
    presentation === TaskPresentationStatus.PAUSED_WAITING
      ? `<button onclick="window.__taskRetry('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-blue-500 hover:bg-blue-600 text-white font-medium rounded-xl transition-all shadow-lg shadow-blue-500/20 text-xs"><span class="material-symbols-outlined text-base">replay</span><span class="font-bold" style="letter-spacing:0.15em">重跑</span></button>`
      : '';
  const btnStop =
    presentation === TaskPresentationStatus.RUNNING ||
    presentation === TaskPresentationStatus.WAITING_NEXT_ROUND ||
    presentation === TaskPresentationStatus.PAUSED_MANUAL ||
    presentation === TaskPresentationStatus.PAUSED_WAITING
      ? `<button onclick="window.__taskStop('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-white text-rose-500 border border-rose-200 font-medium rounded-xl hover:bg-rose-50 transition-all text-xs"><span class="material-symbols-outlined text-base">stop</span><span class="font-bold" style="letter-spacing:0.15em">停止</span></button>`
      : '';

  // interval_waiting 状态横幅
  const intervalBanner = (() => {
    if (
      (!intervalWaiting && !intervalPaused) ||
      !summary?.next_round_at ||
      !summary?.interval_minute
    ) {
      return '';
    }

    const countdown = getCountdownState(summary);
    const waitingExpired = countdown?.expired ?? true;
    const isPausedState = intervalPaused;
    const title = isPausedState ? '等待已暂停' : '等待执行下一轮';
    const subtitle = waitingExpired ? '已到执行时间' : `间隔 ${summary.interval_minute} 分钟`;
    const bannerCls = isPausedState
      ? 'bg-gradient-to-r from-amber-50 to-orange-50 border-amber-200/60'
      : 'bg-gradient-to-r from-violet-50 to-indigo-50 border-violet-200/60';
    const iconBgCls = isPausedState ? 'bg-amber-100' : 'bg-violet-100';
    const iconTextCls = isPausedState ? 'text-amber-500' : 'text-violet-500';
    const titleTextCls = isPausedState ? 'text-amber-700' : 'text-violet-700';
    const subTextCls = isPausedState ? 'text-amber-500' : 'text-violet-500';
    const ringTrack = isPausedState ? 'rgb(254 243 199)' : 'rgb(237 233 254)';
    const ringFill = isPausedState ? 'rgb(245 158 11)' : 'rgb(139 92 246)';
    const ringTextCls = isPausedState ? 'text-amber-600' : 'text-violet-600';
    const centerText = countdown?.text ?? (isPausedState ? '可继续' : '即将开始');
    const percent = countdown?.percent ?? 0;

    return `
    <div class="interval-waiting-banner ${bannerCls} rounded-xl border px-5 py-3.5 mb-4 flex items-center justify-between">
      <div class="flex items-center gap-3">
        <div class="interval-waiting-icon w-9 h-9 ${iconBgCls} flex items-center justify-center rounded-lg">
          <span class="material-symbols-outlined ${iconTextCls}" style="font-size:20px">${isPausedState ? 'pause_circle' : 'hourglass_top'}</span>
        </div>
        <div>
          <div class="text-[12px] font-bold ${titleTextCls} tracking-tight">${title}</div>
          <div class="text-[11px] ${subTextCls} mt-0.5">${subtitle}</div>
        </div>
      </div>
      <div class="flex items-center gap-3">
        <div class="interval-countdown-ring relative w-12 h-12">
          <svg class="w-12 h-12 -rotate-90" viewBox="0 0 48 48">
            <circle cx="24" cy="24" r="20" fill="none" stroke="${ringTrack}" stroke-width="3"/>
            <circle cx="24" cy="24" r="20" fill="none" stroke="${ringFill}" stroke-width="3"
              stroke-dasharray="${20 * 2 * Math.PI}"
              stroke-dashoffset="${20 * 2 * Math.PI * (1 - percent / 100)}"
              stroke-linecap="round"/>
          </svg>
          <div class="absolute inset-0 flex items-center justify-center">
            <span class="text-[10px] font-black ${ringTextCls} font-mono">${centerText}</span>
          </div>
        </div>
      </div>
    </div>`;
  })();

  return `
    <div class="bg-white rounded-2xl shadow-sm border border-s200 p-5 mb-4 flex items-center justify-between">
      <div class="flex items-center gap-4 min-w-0">
        <div class="w-12 h-12 bg-indigo-500/10 flex items-center justify-center rounded-xl shrink-0">
          <span class="material-symbols-outlined text-indigo-500 text-2xl">hub</span>
        </div>
        <div class="min-w-0">
          <div class="flex items-center gap-2">
            <h2 class="text-lg font-bold tracking-tight text-s800 truncate font-mono">${task.name}</h2>
            <span class="px-2.5 py-0.5 ${badge.bg} ${badge.text} text-[11px] font-semibold rounded-full border ${badge.border} shrink-0">${badge.label}</span>
          </div>
          ${deviceTag}
        </div>
      </div>
      <div class="flex items-center gap-2.5 shrink-0 min-w-[120px] justify-end">
        ${btnStart}${btnPause}${btnResume}${btnRetry}${btnStop}
      </div>
    </div>
    ${intervalBanner}`;
}

async function buildMetrics(task: {
  id: string;
  status: string;
  cities: TaskCity[];
}): Promise<string> {
  const kwTotal = task.cities.reduce((s: number, c: TaskCity) => s + c.total, 0);
  const kwDone = task.cities.reduce((s: number, c: TaskCity) => s + c.done, 0);
  const pctDone = kwTotal > 0 ? Math.round((kwDone / kwTotal) * 100) : 0;

  let lastRunLabel = '--';
  let todayRuns = 0;
  let todayKeywords = 0;
  let todayDurationSec = 0;
  try {
    const needsRefresh =
      !_statsCache || _statsCache.taskId !== task.id || _statsCache.taskStatus !== task.status;
    if (needsRefresh) {
      const stats = await invoke<TaskRunStats>('get_task_run_stats', { taskId: task.id });
      _statsCache = { taskId: task.id, stats, taskStatus: task.status };
    }
    todayRuns = _statsCache!.stats.today_runs;
    todayKeywords = _statsCache!.stats.today_keywords;
    todayDurationSec = _statsCache!.stats.today_duration_sec;
    if (_statsCache!.stats.last_run_at) {
      const r = formatRunTime(_statsCache!.stats.last_run_at);
      lastRunLabel = `${r.date} ${r.time}`;
    }
  } catch {
    /* ignore */
  }

  const durationH = Math.floor(todayDurationSec / 3600);
  const durationM = Math.floor((todayDurationSec % 3600) / 60);
  const durationLabel = durationH > 0 ? `${durationH}h${durationM}m` : `${durationM}m`;
  const ratePerHour =
    todayDurationSec > 60 ? Math.round((todayKeywords / todayDurationSec) * 3600) : 0;

  return `
    <div class="grid grid-cols-4 gap-2.5 mb-4">
      <div class="bg-emerald-50/50 rounded-md border border-emerald-100 p-3">
        <div class="flex items-center justify-between mb-1.5">
          <span class="material-symbols-outlined icon-sm text-emerald-400">check_circle</span>
          <span class="text-[11px] font-black text-emerald-600 font-mono">${pctDone}%</span>
        </div>
        <div class="text-[13px] font-bold text-s800 font-mono">${kwDone} <span class="text-s400">/ ${kwTotal}</span></div>
        <div class="text-[11px] text-s500 font-bold uppercase mt-1">完成进度</div>
        <div class="w-full h-2 bg-emerald-100 rounded-full overflow-hidden mt-1.5">
          <div class="h-full bg-emerald-500 rounded-full transition-all" style="width:${pctDone}%"></div>
        </div>
      </div>
      <div class="bg-blue-50/50 rounded-md border border-blue-100 p-3">
        <div class="flex items-center justify-between mb-1.5">
          <span class="material-symbols-outlined icon-sm text-blue-400">trending_up</span>
          <span class="text-[11px] font-black text-blue-600 font-mono">${ratePerHour > 0 ? `${ratePerHour}/h` : '--'}</span>
        </div>
        <div class="text-[13px] font-bold text-s800 font-mono">${todayKeywords} <span class="text-s400 font-sans">词</span></div>
        <div class="text-[11px] text-s500 font-bold uppercase mt-1">今日采集</div>
      </div>
      <div class="bg-violet-50/50 rounded-md border border-violet-100 p-3">
        <div class="flex items-center justify-between mb-1.5">
          <span class="material-symbols-outlined icon-sm text-violet-400">timer</span>
          <span class="text-[11px] font-black text-violet-600 font-mono">${todayRuns} 次</span>
        </div>
        <div class="text-[13px] font-bold text-s800 font-mono">${durationLabel}</div>
        <div class="text-[11px] text-s500 font-bold uppercase mt-1">今日时长</div>
      </div>
      <div class="bg-amber-50/50 rounded-md border border-amber-100 p-3">
        <div class="flex items-center justify-between mb-1.5">
          <span class="material-symbols-outlined icon-sm text-amber-400">schedule</span>
        </div>
        <div class="text-[13px] font-bold text-s800 font-mono">${lastRunLabel}</div>
        <div class="text-[11px] text-s500 font-bold uppercase mt-1">上次执行</div>
      </div>
    </div>`;
}

function buildCityCards(task: { cities: TaskCity[] }): string {
  return task.cities
    .map((c: TaskCity, i: number) => {
      const isPending = c.status === CityStatus.PENDING;
      const isSelected = i === activeCityIdx;
      const isExecuting = c.status === CityStatus.ACTIVE;
      const borderCls = isSelected
        ? isExecuting
          ? 'border-2 border-blue-500 shadow-md'
          : 'border-2 border-green-400 shadow-md'
        : 'border border-s200';
      const statusIcon =
        c.status === CityStatus.DONE
          ? '<span class="material-symbols-outlined text-green-500 icon-sm fill-1">check_circle</span>'
          : c.status === CityStatus.ACTIVE
            ? '<div class="h-1.5 w-1.5 rounded-full bg-blue-500"></div>'
            : '<span class="material-symbols-outlined text-s300 icon-sm">schedule</span>';
      const nameWeight = isSelected ? 'font-bold text-s900' : 'font-semibold text-s500';
      const barBg =
        c.status === CityStatus.DONE
          ? 'bg-green-50'
          : c.status === CityStatus.ACTIVE
            ? 'bg-s100'
            : 'bg-s50';
      const barFill =
        c.status === CityStatus.DONE
          ? 'bg-green-500'
          : c.status === CityStatus.ACTIVE
            ? 'bg-blue-500'
            : 'bg-s200';
      const pct = c.status === CityStatus.DONE ? 100 : c.progress;
      const statsLabel =
        c.status === CityStatus.DONE
          ? '<span class="text-[11px] text-s400 font-bold uppercase">已完成</span>'
          : c.status === CityStatus.ACTIVE
            ? `<span class="text-[11px] text-s400 font-bold uppercase">${c.done}/${c.total} 关键词</span>`
            : '<span class="text-[11px] text-s400 font-bold uppercase">等待中</span>';
      const pctLabel =
        c.status === CityStatus.DONE
          ? '<span class="text-[11px] font-black text-green-600 font-mono">100%</span>'
          : c.status === CityStatus.ACTIVE
            ? `<span class="text-[11px] font-black text-blue-600 font-mono">${c.progress}%</span>`
            : '';
      const cardBg =
        c.status === CityStatus.DONE
          ? 'bg-green-50/50'
          : c.status === CityStatus.ACTIVE
            ? 'bg-blue-50/40'
            : 'bg-s50/50';
      const dragAttr = '';
      const dragCls = isPending ? 'city-draggable' : '';
      const dragHandle = isPending
        ? '<span class="material-symbols-outlined text-s300 text-sm cursor-grab city-drag-handle">drag_indicator</span>'
        : '';
      return `
      <div class="city-card ${cardBg} rounded-md ${borderCls} p-3 cursor-pointer ${!isSelected ? 'hover:bg-s50' : ''} transition-all relative overflow-hidden ${dragCls}" style="width:220px;min-width:220px;flex-shrink:0" data-city-name="${esc(c.name)}" data-city-idx="${i}" ${dragAttr} onclick="window.__switchCity(${i})">
        <div class="flex items-center justify-between mb-2">
          <div class="flex items-center space-x-2">
            ${dragHandle}
            <span class="material-symbols-outlined icon-sm text-blue-400">location_city</span>
            <span class="text-[13px] ${nameWeight}">${c.name}</span>
          </div>
          ${statusIcon}
        </div>
        <div class="text-[10px] text-s500 truncate mb-2" title="${c.poi}">
          <span class="material-symbols-outlined icon-xs text-s300 align-middle mr-0.5">location_on</span>${c.poi}
        </div>
        <div class="w-full h-2 ${barBg} rounded-full overflow-hidden">
          <div class="h-full ${barFill} rounded-full" style="width: ${pct}%"></div>
        </div>
        <div class="flex justify-between mt-2">
          ${statsLabel}
          ${pctLabel}
        </div>
      </div>`;
    })
    .join('');
}

let _sortableInstance: Sortable | null = null;

function bindCityDragEvents(taskId: string) {
  const container = document.getElementById('tv-cities');
  if (!container) return;

  // 销毁旧实例（防止重复绑定）
  _sortableInstance?.destroy();

  _sortableInstance = Sortable.create(container, {
    animation: 200,
    easing: 'cubic-bezier(0.25, 1, 0.5, 1)',
    handle: '.city-drag-handle',
    draggable: '.city-draggable',
    ghostClass: 'city-ghost',
    chosenClass: 'city-chosen',
    dragClass: 'city-drag',
    filter: '.city-card:not(.city-draggable)',
    preventOnFilter: false,
    forceFallback: true,
    fallbackClass: 'city-fallback',
    fallbackOnBody: false,
    onEnd: () => {
      // 收集 pending 城市新顺序
      const newOrder = ([...container.querySelectorAll('.city-draggable')] as HTMLElement[])
        .map(c => c.dataset.cityName || '')
        .filter(Boolean);

      invoke('engine_reorder_cities', { taskId, newOrder }).catch(err =>
        console.error('[sortable] reorder failed:', err),
      );
    },
  });
}

function buildKeywordGrid(city: TaskCity): string {
  return city.keywords
    .map((k: TaskKeyword) => {
      if (k.status === KeywordStatus.OK) {
        return `<div class="kw-item flex items-center justify-between p-2 rounded bg-green-100/50 border border-green-200 transition-all hover:bg-green-100">
        <div class="flex items-center min-w-0">
          <span class="material-symbols-outlined icon-sm text-green-500 mr-1.5 fill-1">check_circle</span>
          <span class="text-[12px] font-semibold text-s600 truncate">${k.name}</span>
        </div>
      </div>`;
      } else if (k.status === KeywordStatus.RUN) {
        return `<div class="kw-item flex items-center p-2 rounded bg-blue-100/50 border border-blue-400 keyword-active ring-2 ring-blue-100">
        <div class="h-2 w-2 rounded-full bg-blue-500 mr-1.5 animate-pulse"></div>
        <span class="text-[12px] font-bold text-blue-700 truncate">${k.name}</span>
      </div>`;
      } else {
        return `<div class="kw-item flex items-center p-2 rounded bg-s100/50 border border-s200 hover:border-s300 transition-all cursor-pointer">
        <span class="text-[12px] font-medium text-s500 truncate">${k.name}</span>
      </div>`;
      }
    })
    .join('');
}

/** 注册全局视图切换回调 */
export function registerViewActions() {
  const actions: Record<string, (...args: unknown[]) => void> = {
    __switchTask: (taskId: unknown) => {
      const task = globalQueue.find(t => t.id === (taskId as string));
      if (!task || task === activeTask) return;
      setActiveTask(task);
      setActiveTaskDetail(null);
      setActiveCityIdx(0);
      setSelectedDevice(null);
      _onUpdateCardSelection?.();
      renderTaskView();
      _onLoadChainForDevice?.('');
    },

    __switchCity: async (idx: unknown) => {
      const i = idx as number;
      if (i === activeCityIdx) return;
      setActiveCityIdx(i);
      await renderTaskView();
      const cityContainer = document.getElementById('tv-cities');
      const cards = cityContainer?.querySelectorAll('[onclick*="__switchCity"]');
      if (cards && cards[i]) {
        cards[i].scrollIntoView({ behavior: 'smooth', block: 'nearest', inline: 'center' });
      }
    },

    __filterKw: (val: unknown) => {
      const q = (val as string).toLowerCase();
      document.querySelectorAll('#tv-kw-grid .kw-item').forEach(el => {
        const name = el.textContent?.toLowerCase() || '';
        (el as HTMLElement).style.display = name.includes(q) ? '' : 'none';
      });
    },
  };

  for (const [name, fn] of Object.entries(actions)) {
    (window as unknown as Record<string, unknown>)[name] = fn;
  }
}
