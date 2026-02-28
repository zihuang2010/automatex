import { invoke } from '@tauri-apps/api/core';
import { TaskStatus, CityStatus, KeywordStatus } from './constants';
import { TaskRunStats, TaskCity, TaskKeyword } from './types';
import {
    activeTask,
    activeCityIdx,
    setActiveTask,
    setActiveCityIdx,
    globalQueue,
    setSelectedDevice,
} from './state';
import { $, esc, formatRunTime } from './utils';

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

/* ===== Task View Rendering ===== */

// TaskRunStats 缓存（避免每次渲染都跨进程查 DB）
let _statsCache: { taskId: string; stats: TaskRunStats; taskStatus: string } | null = null;

export async function loadTasksForDevice(serial: string) {
    // 查找分配给该设备的任务并切换
    const task = globalQueue.find(t => t.assigned_device === serial);
    if (task && task !== activeTask) {
        setActiveTask(task);
        setActiveCityIdx(0);
    }
    await renderTaskView();
    if (serial) {
        _onLoadChainForDevice?.(serial);
    }
}

export async function renderTaskView() {
    const mid = $('#col-mid')!;
    const task = activeTask;
    if (!task) {
        mid.innerHTML =
            '<div class="empty-hint" style="padding:40px;text-align:center">选择任务以查看详情</div>';
        return;
    }

    const city = task.cities[activeCityIdx] ?? task.cities[0];
    if (!city) {
        mid.innerHTML = '<div class="empty-hint">无城市数据</div>';
        return;
    }

    // ── Status Badge ──
    type BadgeInfo = { bg: string; text: string; border: string; label: string };
    const badgeMap: Record<string, BadgeInfo> = {
        [TaskStatus.WAITING]: {
            bg: 'bg-gray-100',
            text: 'text-gray-600',
            border: 'border-gray-200',
            label: '等待中',
        },
        [TaskStatus.EXECUTING]: {
            bg: 'bg-blue-100',
            text: 'text-blue-700',
            border: 'border-blue-200',
            label: '执行中',
        },
        [TaskStatus.PAUSED]: {
            bg: 'bg-amber-100',
            text: 'text-amber-700',
            border: 'border-amber-200',
            label: '已暂停',
        },
        [TaskStatus.SUCCESS]: {
            bg: 'bg-green-100',
            text: 'text-green-700',
            border: 'border-green-200',
            label: '已完成',
        },
        [TaskStatus.ERROR]: {
            bg: 'bg-red-100',
            text: 'text-red-700',
            border: 'border-red-200',
            label: '异常',
        },
    };
    const badge = badgeMap[task.status] || badgeMap[TaskStatus.WAITING];

    // ── Device assignment tag ──
    const deviceTag = task.assigned_device
        ? `<p class="text-[11px] text-slate-400 font-semibold mt-0.5"><span class="material-symbols-outlined text-sm align-middle mr-0.5">smartphone</span>${esc(task.assigned_device)}</p>`
        : '';

    // ── Metric Cards ──
    const kwTotal = task.cities.reduce((s: number, c: TaskCity) => s + c.total, 0);
    const kwDone = task.cities.reduce((s: number, c: TaskCity) => s + c.done, 0);

    let lastRunLabel = '--';
    let todayRuns = 0;
    try {
        const needsRefresh =
            !_statsCache ||
            _statsCache.taskId !== task.id ||
            _statsCache.taskStatus !== task.status;
        if (needsRefresh) {
            const stats = await invoke<TaskRunStats>('get_task_run_stats', { taskId: task.id });
            _statsCache = { taskId: task.id, stats, taskStatus: task.status };
        }
        todayRuns = _statsCache!.stats.today_runs;
        if (_statsCache!.stats.last_run_at) {
            const r = formatRunTime(_statsCache!.stats.last_run_at);
            lastRunLabel = `${r.date} ${r.time}`;
        }
    } catch {
        /* ignore */
    }

    const metricCards = `
    <div class="grid grid-cols-3 gap-2.5 mb-4 shrink-0">
      <div class="bg-white border border-slate-200 p-4 rounded-2xl flex items-center gap-3.5">
        <div class="p-2.5 bg-emerald-50 rounded-xl shrink-0">
          <span class="material-symbols-outlined text-emerald-500">check_circle</span>
        </div>
        <div>
          <div class="text-lg font-bold text-slate-600">${kwDone} <span class="text-xs font-bold text-slate-400">/ ${kwTotal}</span></div>
          <div class="text-[11px] font-bold text-slate-500">完成进度</div>
        </div>
      </div>
      <div class="bg-white border border-slate-200 p-4 rounded-2xl flex items-center gap-3.5">
        <div class="p-2.5 bg-blue-50 rounded-xl shrink-0">
          <span class="material-symbols-outlined text-blue-500">schedule</span>
        </div>
        <div>
          <div class="text-sm font-bold text-slate-600">${lastRunLabel}</div>
          <div class="text-[11px] font-bold text-slate-500">上次执行</div>
        </div>
      </div>
      <div class="bg-white border border-slate-200 p-4 rounded-2xl flex items-center gap-3.5">
        <div class="p-2.5 bg-indigo-50 rounded-xl shrink-0">
          <span class="material-symbols-outlined text-indigo-500">refresh</span>
        </div>
        <div>
          <div class="text-lg font-bold text-slate-600">${todayRuns} <span class="text-xs font-bold text-slate-600">次</span></div>
          <div class="text-[11px] font-bold text-slate-500">今日执行</div>
        </div>
      </div>
    </div>`;

    // ── Action Buttons ──
    const btnStart =
        task.status === TaskStatus.WAITING
            ? `<button onclick="window.__taskStart('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-emerald-500 hover:bg-emerald-600 text-white font-medium rounded-xl transition-all shadow-lg shadow-emerald-500/20 text-xs"><span class="material-symbols-outlined text-base">play_arrow</span><span class="font-bold" style="letter-spacing:0.15em">启动</span></button>`
            : '';
    const btnPause =
        task.status === TaskStatus.EXECUTING
            ? `<button onclick="window.__taskPause('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-amber-500 hover:bg-amber-600 text-white font-medium rounded-xl transition-all shadow-lg shadow-amber-500/20 text-xs"><span class="material-symbols-outlined text-base">pause</span><span class="font-bold" style="letter-spacing:0.15em">暂停</span></button>`
            : '';
    const btnResume =
        task.status === TaskStatus.PAUSED || task.status === TaskStatus.ERROR
            ? `<button onclick="window.__taskResume('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-emerald-500 hover:bg-emerald-600 text-white font-medium rounded-xl transition-all shadow-lg shadow-emerald-500/20 text-xs"><span class="material-symbols-outlined text-base">play_arrow</span><span class="font-bold" style="letter-spacing:0.15em">继续</span></button>`
            : '';
    const btnRetry =
        task.status === TaskStatus.ERROR || task.status === TaskStatus.SUCCESS
            ? `<button onclick="window.__taskRetry('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-blue-500 hover:bg-blue-600 text-white font-medium rounded-xl transition-all shadow-lg shadow-blue-500/20 text-xs"><span class="material-symbols-outlined text-base">replay</span><span class="font-bold" style="letter-spacing:0.15em">重跑</span></button>`
            : '';
    const btnStop =
        task.status === TaskStatus.EXECUTING || task.status === TaskStatus.PAUSED
            ? `<button onclick="window.__taskStop('${task.id}')" class="flex items-center gap-2.5 px-5 py-2 bg-white text-rose-500 border border-rose-200 font-medium rounded-xl hover:bg-rose-50 transition-all text-xs"><span class="material-symbols-outlined text-base">stop</span><span class="font-bold" style="letter-spacing:0.15em">停止</span></button>`
            : '';

    // ── City Cards ──
    const cityCards = task.cities
        .map((c: TaskCity, i: number) => {
            const isActive = i === activeCityIdx;
            const borderCls = isActive
                ? 'border-2 border-blue-500 shadow-md'
                : 'border border-slate-200';
            const statusIcon =
                c.status === CityStatus.DONE
                    ? '<span class="material-symbols-outlined text-green-500 icon-sm fill-1">check_circle</span>'
                    : c.status === CityStatus.ACTIVE
                      ? `<div class="h-1.5 w-1.5 rounded-full bg-blue-500"></div>`
                      : '<span class="material-symbols-outlined text-slate-300 icon-sm">schedule</span>';
            const nameWeight = isActive
                ? 'font-bold text-slate-900'
                : 'font-semibold text-slate-500';
            const barBg =
                c.status === CityStatus.DONE
                    ? 'bg-green-50'
                    : c.status === CityStatus.ACTIVE
                      ? 'bg-slate-100'
                      : 'bg-slate-50';
            const barFill =
                c.status === CityStatus.DONE
                    ? 'bg-green-500'
                    : c.status === CityStatus.ACTIVE
                      ? 'bg-blue-500'
                      : 'bg-slate-200';
            const pct = c.status === CityStatus.DONE ? 100 : c.progress;
            const statsLabel =
                c.status === CityStatus.DONE
                    ? '<span class="text-[11px] text-slate-400 font-bold uppercase">已完成</span>'
                    : c.status === CityStatus.ACTIVE
                      ? `<span class="text-[11px] text-slate-400 font-bold uppercase">${c.done}/${c.total} 关键词</span>`
                      : '<span class="text-[11px] text-slate-400 font-bold uppercase">等待中</span>';
            const pctLabel =
                c.status === CityStatus.DONE
                    ? '<span class="text-[11px] font-black text-green-600">100%</span>'
                    : c.status === CityStatus.ACTIVE
                      ? `<span class="text-[11px] font-black text-blue-600">${c.progress}%</span>`
                      : '';
            const cardBg =
                c.status === CityStatus.DONE
                    ? 'bg-green-50/50'
                    : c.status === CityStatus.ACTIVE
                      ? 'bg-blue-50/40'
                      : 'bg-slate-50/50';
            return `
        <div class="${cardBg} rounded-md ${borderCls} p-3 cursor-pointer ${!isActive ? 'hover:bg-slate-50' : ''} transition-all relative overflow-hidden" style="width:220px;min-width:220px;flex-shrink:0" onclick="window.__switchCity(${i})">
          <div class="flex items-center justify-between mb-2">
            <div class="flex items-center space-x-2">
              <span class="material-symbols-outlined icon-sm text-blue-400">location_city</span>
              <span class="text-[13px] ${nameWeight}">${c.name}</span>
            </div>
            ${statusIcon}
          </div>
          <div class="text-[10px] text-slate-500 truncate mb-2" title="${c.poi}">
            <span class="material-symbols-outlined icon-xs text-slate-300 align-middle mr-0.5">location_on</span>${c.poi}
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

    // ── Keyword Grid ──
    const kwGrid = city.keywords
        .map((k: TaskKeyword) => {
            if (k.status === KeywordStatus.OK) {
                return `<div class="kw-item flex items-center justify-between p-2 rounded bg-green-100/50 border border-green-200 transition-all hover:bg-green-100">
        <div class="flex items-center min-w-0">
          <span class="material-symbols-outlined icon-sm text-green-500 mr-1.5 fill-1">check_circle</span>
          <span class="text-[12px] font-semibold text-slate-600 truncate">${k.name}</span>
        </div>
      </div>`;
            } else if (k.status === KeywordStatus.RUN) {
                return `<div class="kw-item flex items-center p-2 rounded bg-blue-100/50 border border-blue-400 keyword-active ring-2 ring-blue-100">
        <div class="h-2 w-2 rounded-full bg-blue-500 mr-1.5 animate-pulse"></div>
        <span class="text-[12px] font-bold text-blue-700 truncate">${k.name}</span>
      </div>`;
            } else {
                return `<div class="kw-item flex items-center p-2 rounded bg-slate-100/50 border border-slate-200 hover:border-slate-300 transition-all cursor-pointer">
        <span class="text-[12px] font-medium text-slate-500 truncate">${k.name}</span>
      </div>`;
            }
        })
        .join('');

    mid.innerHTML = `
    <!-- Task Header -->
    <div class="bg-white rounded-2xl shadow-sm border border-slate-200 p-5 mb-4 flex items-center justify-between shrink-0">
      <div class="flex items-center gap-4">
        <div class="w-12 h-12 bg-indigo-500/10 flex items-center justify-center rounded-xl shrink-0">
          <span class="material-symbols-outlined text-indigo-500 text-2xl">hub</span>
        </div>
        <div>
          <div class="flex items-center gap-2">
            <h2 class="text-lg font-bold tracking-tight text-slate-800">${task.name}</h2>
            <span class="px-2.5 py-0.5 ${badge.bg} ${badge.text} text-[11px] font-semibold rounded-full border ${badge.border}">${badge.label}</span>
          </div>
          ${deviceTag}
        </div>
      </div>
      <div class="flex items-center gap-2.5">
        ${btnStart}${btnPause}${btnResume}${btnRetry}${btnStop}
      </div>
    </div>

    <!-- Metric Cards -->
    ${metricCards}

    <!-- City Cards -->
    <div class="flex gap-2.5 mb-4 shrink-0 overflow-x-auto pb-1 scrollbar-hide">
      ${cityCards}
    </div>

    <!-- Keywords Section -->
    <div class="flex-1 bg-white rounded-md border border-[var(--panel-border)] shadow-sm overflow-hidden flex flex-col min-h-0">
      <div class="px-4 py-2 bg-slate-50/50 border-b border-slate-100 flex items-center justify-between shrink-0">
        <div class="flex items-center space-x-3">
          <span class="text-[11px] font-black text-slate-500 uppercase tracking-tight">${city.name} ｜ 关键词 </span>
          <span class="px-1.5 py-0.5 bg-slate-200/50 text-slate-600 rounded text-[10px] font-black">共 ${city.total} 个</span>
        </div>
        <div class="relative w-56">
          <span class="material-symbols-outlined absolute left-2.5 top-1/2 -translate-y-1/2 text-slate-400 text-sm">search</span>
          <input class="w-full pl-9 pr-3 py-1.5 bg-white border border-slate-200 rounded-md text-xs focus:ring-2 focus:ring-blue-100 focus:border-blue-500 outline-none transition-all" placeholder="搜索关键词..." type="text" id="kw-filter-input" oninput="window.__filterKw(this.value)" />
        </div>
      </div>
      <div class="p-4 overflow-y-auto flex-1">
        <div class="flex flex-wrap gap-2" id="kw-grid">
          ${kwGrid}
        </div>
      </div>
    </div>
  `;
}

/** 注册全局视图切换回调 */
export function registerViewActions() {
    const actions: Record<string, (...args: unknown[]) => void> = {
        __switchTask: (taskId: unknown) => {
            const task = globalQueue.find(t => t.id === (taskId as string));
            if (!task || task === activeTask) return;
            setActiveTask(task);
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
            const cityContainer = document.querySelector('#col-mid .overflow-x-auto');
            const cards = cityContainer?.querySelectorAll('[onclick*="__switchCity"]');
            if (cards && cards[i]) {
                cards[i].scrollIntoView({ behavior: 'smooth', block: 'nearest', inline: 'center' });
            }
        },

        __filterKw: (val: unknown) => {
            const q = (val as string).toLowerCase();
            document.querySelectorAll('#kw-grid .kw-item').forEach(el => {
                const name = el.textContent?.toLowerCase() || '';
                (el as HTMLElement).style.display = name.includes(q) ? '' : 'none';
            });
        },
    };

    for (const [name, fn] of Object.entries(actions)) {
        (window as unknown as Record<string, unknown>)[name] = fn;
    }
}
