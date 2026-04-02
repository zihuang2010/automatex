import Sortable from 'sortablejs';

import { TaskPresentationStatus } from './constants';
import { activeTask, globalQueue, reorderGlobalQueue } from './state';
import { TaskSummary } from './types';
import { $, esc, getCountdownState, getPresentationState } from './utils';

/** 上一次渲染的任务快照，用于快速跳过无变化刷新 (#5) */
let _lastQueueHtml = '';
let _queueSortable: Sortable | null = null;
let _countdownTimer: ReturnType<typeof setInterval> | null = null;

function getTaskCardProgress(task: TaskSummary): { pct: number; label: string } {
  if (task.keyword_total > 0) {
    return {
      pct: task.progress,
      label: `总进度 · ${task.keyword_done}/${task.keyword_total}`,
    };
  }
  return {
    pct: task.progress,
    label: '任务总进度',
  };
}

/**
 * 倒计时秒级刷新器（纯 DOM patch，不触发完整 re-render，避免闪烁）
 *
 * 每秒只更新已有的倒计时文字 + 进度条百分比值。
 * 当没有 interval_waiting 任务时自动停止。
 */
function startCountdownTicker() {
  if (_countdownTimer) return;
  _countdownTimer = setInterval(() => {
    const waitingTasks = globalQueue.filter(
      q =>
        (getPresentationState(q) === TaskPresentationStatus.WAITING_NEXT_ROUND ||
          getPresentationState(q) === TaskPresentationStatus.PAUSED_WAITING) &&
        q.next_round_at &&
        q.next_round_at > 0 &&
        q.interval_minute &&
        q.interval_minute > 0,
    );

    if (waitingTasks.length === 0) {
      clearInterval(_countdownTimer!);
      _countdownTimer = null;
      return;
    }

    // ── 轻量 DOM patch: queue 卡片中的倒计时 ──
    for (const q of waitingTasks) {
      const countdown = getCountdownState(q);
      const card = document.querySelector(
        `.task-queue-item[data-task-id="${encodeURIComponent(q.id)}"]`,
      );
      if (!card) continue;

      // 更新倒计时文字
      const textEl = card.querySelector('.interval-countdown-text');
      if (textEl) textEl.textContent = countdown?.text ?? '即将开始';

      // 更新进度条宽度
      const barEl = card.querySelector('.interval-countdown-bar-fill') as HTMLElement;
      if (barEl) barEl.style.width = `${countdown?.percent ?? 0}%`;
    }

    // ── 轻量 DOM patch: task-view 中的倒计时 banner ──
    const ring = document.querySelector('.interval-countdown-ring');
    if (ring && waitingTasks.length > 0) {
      const activeWaiting =
        activeTask &&
        (getPresentationState(activeTask) === TaskPresentationStatus.WAITING_NEXT_ROUND ||
          getPresentationState(activeTask) === TaskPresentationStatus.PAUSED_WAITING) &&
        activeTask.next_round_at &&
        activeTask.interval_minute
          ? activeTask
          : null;
      if (activeWaiting && document.querySelector('.interval-waiting-banner')) {
        const countdown = getCountdownState(activeWaiting);
        // 更新环内文字
        const ringText = ring.querySelector('span');
        if (ringText) {
          ringText.textContent = countdown?.text ?? '即将开始';
        }
        // 更新 SVG 环形进度
        const circle = ring.querySelector('svg circle:last-child') as SVGCircleElement;
        if (circle) {
          const r = 20;
          const circumference = r * 2 * Math.PI;
          circle.setAttribute(
            'stroke-dashoffset',
            String(circumference * (1 - (countdown?.percent ?? 0) / 100)),
          );
        }
      }
    }
  }, 1000);
}

/* ===== Task Queue (Right Column) ===== */

export function loadChainForDevice(_serial: string) {
  const cards = $('#chain-cards')!;

  const queueSub = document.querySelector('.queue-sub');
  const executingCount = globalQueue.filter(
    t => getPresentationState(t) === TaskPresentationStatus.RUNNING,
  ).length;
  if (queueSub)
    queueSub.textContent = `${executingCount} 个执行中 · 共 ${globalQueue.length} 个任务`;

  const newHtml = globalQueue
    .map((q: TaskSummary) => {
      const isActive = activeTask ? q.id === activeTask.id : false;
      const kwTotal = q.keyword_total;
      const cityCount = q.city_count;
      const taskIdAttr = encodeURIComponent(q.id);
      const clickTaskId = JSON.stringify(q.id);
      const presentation = getPresentationState(q);
      const ringColor =
        presentation === TaskPresentationStatus.ERROR_PAUSED
          ? 'ring-red-200'
          : presentation === TaskPresentationStatus.PAUSED_MANUAL ||
              presentation === TaskPresentationStatus.PAUSED_WAITING
            ? 'ring-amber-200'
            : presentation === TaskPresentationStatus.COMPLETED
              ? 'ring-green-200'
              : 'ring-blue-200';
      const ring = isActive ? `ring-2 ${ringColor}` : '';

      const statsRow = `
        <div class="flex items-center gap-4 text-s500">
          <div class="flex items-center gap-1">
            <span class="material-symbols-outlined icon-xs text-[14px]">domain</span>
            <span class="text-[10px] font-semibold leading-none">${cityCount} 城市</span>
          </div>
          <div class="flex items-center gap-1">
            <span class="material-symbols-outlined icon-xs text-[14px]">sell</span>
            <span class="text-[10px] font-semibold leading-none">${kwTotal} 关键词</span>
          </div>
        </div>`;

      if (presentation === TaskPresentationStatus.WAITING_NEXT_ROUND) {
        const countdown = getCountdownState(q);
        const countdownText = countdown?.text ?? '即将开始';
        const remainPct = countdown?.percent ?? 0;
        return `
      <div class="task-queue-item bg-violet-50/40 border border-violet-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all duration-200 hover:shadow-md hover:-translate-y-0.5 ${ring}" data-task-id="${taskIdAttr}" onclick='window.__switchTask(${clickTaskId})'>
        <div class="w-1 self-stretch bg-violet-500"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex items-center justify-between gap-3 mb-2">
            <div class="flex min-w-0 items-center gap-2.5 pr-2">
              <span class="task-queue-drag-handle material-symbols-outlined">drag_indicator</span>
              <h4 class="truncate text-[12px] font-bold leading-none tracking-[0.01em] text-s700">${esc(q.name)}</h4>
            </div>
            <div class="interval-waiting-badge flex h-7 shrink-0 items-center gap-1.5 rounded-full bg-violet-100 px-2.5 py-0.5">
              <span class="interval-waiting-dot w-1.5 h-1.5 bg-violet-500 rounded-full"></span>
              <span class="text-[11px] font-bold text-violet-600">等待中</span>
            </div>
          </div>
          ${statsRow}
          <div class="interval-countdown-bar mt-2 flex items-center gap-2.5">
            <div class="flex items-center gap-1.5 shrink-0">
              <span class="material-symbols-outlined text-violet-400" style="font-size:14px">hourglass_top</span>
              <span class="interval-countdown-text text-[11px] font-black text-violet-600 font-mono tracking-wide">${countdownText}</span>
            </div>
            <div class="flex-1 h-[3px] bg-violet-100 rounded-full overflow-hidden">
              <div class="interval-countdown-bar-fill h-full bg-violet-400 rounded-full" style="width:${remainPct}%"></div>
            </div>
          </div>
        </div>
      </div>`;
      } else if (presentation === TaskPresentationStatus.RUNNING) {
        const { pct, label: progressLabel } = getTaskCardProgress(q);
        return `
      <div class="task-queue-item bg-blue-50/40 border border-blue-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all duration-200 hover:shadow-md hover:-translate-y-0.5 ${ring}" data-task-id="${taskIdAttr}" onclick='window.__switchTask(${clickTaskId})'>
        <div class="w-1 self-stretch bg-[#2563EB]"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex items-center justify-between gap-3 mb-2">
            <div class="flex min-w-0 items-center gap-2.5 pr-2">
              <span class="task-queue-drag-handle material-symbols-outlined">drag_indicator</span>
              <h4 class="truncate text-[12px] font-bold leading-none tracking-[0.01em] text-s700">${esc(q.name)}</h4>
            </div>
            <div class="flex h-7 shrink-0 items-center gap-1.5 rounded-full bg-s100 px-2.5 py-0.5">
              <span class="w-1.5 h-1.5 bg-[#2563EB] rounded-full animate-pulse"></span>
              <span class="text-[11px] font-bold text-[#2563EB]">执行中</span>
            </div>
          </div>
          ${statsRow}
          <div class="flex justify-between items-center mt-2">
            <span class="text-[10px] font-semibold text-s400">${esc(progressLabel)}</span>
            <span class="text-[10px] font-bold text-[#2563EB]">${pct}%</span>
          </div>
          <div class="absolute bottom-0 left-1 right-0 h-[2px] bg-s100 overflow-hidden">
            <div class="h-full bg-[#2563EB]" style="width: ${pct}%"></div>
          </div>
        </div>
      </div>`;
      } else if (presentation === TaskPresentationStatus.PAUSED_WAITING) {
        const { pct, label: progressLabel } = getTaskCardProgress(q);
        const countdown = getCountdownState(q);
        const countdownText = countdown?.text ?? '可继续';
        const remainPct = countdown?.percent ?? 0;
        return `
      <div class="task-queue-item bg-amber-50/50 border border-amber-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all duration-200 hover:shadow-md hover:-translate-y-0.5 ${ring}" data-task-id="${taskIdAttr}" onclick='window.__switchTask(${clickTaskId})'>
        <div class="w-1 self-stretch bg-amber-400"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex items-center justify-between gap-3 mb-2">
            <div class="flex min-w-0 items-center gap-2.5 pr-2">
              <span class="task-queue-drag-handle material-symbols-outlined">drag_indicator</span>
              <h4 class="truncate text-[12px] font-bold leading-none tracking-[0.01em] text-s700">${esc(q.name)}</h4>
            </div>
            <div class="flex h-7 shrink-0 items-center gap-1.5 rounded-full bg-amber-100 px-2.5 py-0.5">
              <span class="w-1.5 h-1.5 bg-amber-500 rounded-full"></span>
              <span class="text-[11px] font-bold text-amber-700">等待暂停</span>
            </div>
          </div>
          ${statsRow}
          <div class="interval-countdown-bar mt-2 flex items-center gap-2.5">
            <div class="flex items-center gap-1.5 shrink-0">
              <span class="material-symbols-outlined text-amber-500" style="font-size:14px">schedule</span>
              <span class="interval-countdown-text text-[11px] font-black text-amber-700 font-mono tracking-wide">${countdownText}</span>
            </div>
            <div class="flex-1 h-[3px] bg-amber-100 rounded-full overflow-hidden">
              <div class="interval-countdown-bar-fill h-full bg-amber-400 rounded-full" style="width:${remainPct}%"></div>
            </div>
            <span class="text-[10px] font-semibold text-amber-500">恢复后继续</span>
          </div>
          <div class="flex justify-between items-center mt-2">
            <span class="text-[10px] font-semibold text-s400">${esc(progressLabel)}</span>
            <span class="text-[10px] font-bold text-amber-600">${pct}%</span>
          </div>
          <div class="absolute bottom-0 left-1 right-0 h-[2px] bg-s100 overflow-hidden">
            <div class="h-full bg-amber-400" style="width: ${pct}%"></div>
          </div>
        </div>
      </div>`;
      } else if (presentation === TaskPresentationStatus.PAUSED_MANUAL) {
        const { pct, label: progressLabel } = getTaskCardProgress(q);
        return `
      <div class="task-queue-item bg-amber-50/40 border border-amber-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all duration-200 hover:shadow-md hover:-translate-y-0.5 ${ring}" data-task-id="${taskIdAttr}" onclick='window.__switchTask(${clickTaskId})'>
        <div class="w-1 self-stretch bg-amber-400"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex items-center justify-between gap-3 mb-2">
            <div class="flex min-w-0 items-center gap-2.5 pr-2">
              <span class="task-queue-drag-handle material-symbols-outlined">drag_indicator</span>
              <h4 class="truncate text-[12px] font-bold leading-none tracking-[0.01em] text-s700">${esc(q.name)}</h4>
            </div>
            <div class="flex h-7 shrink-0 items-center rounded-full bg-s100 px-2.5 py-0.5">
              <span class="text-[11px] font-bold text-amber-600">已暂停</span>
            </div>
          </div>
          ${statsRow}
          <div class="flex justify-between items-center mt-2">
            <span class="text-[10px] font-semibold text-s400">${esc(progressLabel)}</span>
            <span class="text-[10px] font-bold text-amber-600">${pct}%</span>
          </div>
          <div class="absolute bottom-0 left-1 right-0 h-[2px] bg-s100 overflow-hidden">
            <div class="h-full bg-amber-400" style="width: ${pct}%"></div>
          </div>
        </div>
      </div>`;
      } else if (presentation === TaskPresentationStatus.READY) {
        return `
      <div class="task-queue-item bg-s50/60 border border-s200 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all duration-200 hover:shadow-md hover:-translate-y-0.5 ${ring}" data-task-id="${taskIdAttr}" onclick='window.__switchTask(${clickTaskId})'>
        <div class="w-1 self-stretch bg-[#64748B]"></div>
        <div class="flex-1 p-3">
          <div class="flex items-center justify-between gap-3 mb-2">
            <div class="flex min-w-0 items-center gap-2.5 pr-2">
              <span class="task-queue-drag-handle material-symbols-outlined">drag_indicator</span>
              <h4 class="truncate text-[12px] font-bold leading-none tracking-[0.01em] text-s700">${esc(q.name)}</h4>
            </div>
            <div class="flex h-7 shrink-0 items-center rounded-full bg-s100 px-2.5 py-0.5">
              <span class="text-[11px] font-bold text-[#64748B]">待启动</span>
            </div>
          </div>
          ${statsRow}
        </div>
      </div>`;
      } else if (presentation === TaskPresentationStatus.COMPLETED) {
        return `
      <div class="task-queue-item bg-green-50/40 border border-green-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer opacity-80 transition-all duration-200 hover:opacity-100 hover:-translate-y-0.5 ${ring}" data-task-id="${taskIdAttr}" onclick='window.__switchTask(${clickTaskId})'>
        <div class="w-1 self-stretch bg-[#10B981]"></div>
        <div class="flex-1 p-3">
          <div class="flex items-center justify-between gap-3 mb-2">
            <div class="flex min-w-0 items-center gap-2.5 pr-2">
              <span class="task-queue-drag-handle material-symbols-outlined">drag_indicator</span>
              <h4 class="truncate text-[12px] font-bold leading-none tracking-[0.01em] text-s700">${esc(q.name)}</h4>
            </div>
            <div class="flex h-7 shrink-0 items-center rounded-full bg-s100 px-2.5 py-0.5">
              <span class="text-[11px] font-bold text-[#10B981]">已完成</span>
            </div>
          </div>
          ${statsRow}
        </div>
      </div>`;
      } else if (presentation === TaskPresentationStatus.ERROR_PAUSED) {
        const { pct, label: progressLabel } = getTaskCardProgress(q);
        return `
      <div class="task-queue-item bg-red-50/40 border border-red-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all duration-200 hover:shadow-md hover:-translate-y-0.5 ${ring}" data-task-id="${taskIdAttr}" onclick='window.__switchTask(${clickTaskId})'>
        <div class="w-1 self-stretch bg-red-500"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex items-center justify-between gap-3 mb-2">
            <div class="flex min-w-0 items-center gap-2.5 pr-2">
              <span class="task-queue-drag-handle material-symbols-outlined">drag_indicator</span>
              <h4 class="truncate text-[12px] font-bold leading-none tracking-[0.01em] text-s700">${esc(q.name)}</h4>
            </div>
            <div class="flex h-7 shrink-0 items-center rounded-full bg-s100 px-2.5 py-0.5">
              <span class="text-[11px] font-bold text-red-500">异常暂停</span>
            </div>
          </div>
          ${statsRow}
          <div class="flex justify-between items-center mt-2">
            <span class="text-[10px] font-semibold text-s400">${esc(progressLabel)}</span>
            <span class="text-[10px] font-bold text-red-500">${pct}%</span>
          </div>
          <div class="absolute bottom-0 left-1 right-0 h-[2px] bg-s100 overflow-hidden">
            <div class="h-full bg-red-400" style="width: ${pct}%"></div>
          </div>
        </div>
      </div>`;
      } else {
        return '';
      }
    })
    .join('');

  // #5: 内容未变化时跳过 DOM 更新
  if (newHtml !== _lastQueueHtml) {
    cards.innerHTML = newHtml;
    _lastQueueHtml = newHtml;
  }

  bindTaskQueueDragEvents(cards);

  // interval_waiting 检测：有等待中的任务时启动倒计时 ticker
  const hasIntervalWaiting = globalQueue.some(
    q =>
      (getPresentationState(q) === TaskPresentationStatus.WAITING_NEXT_ROUND ||
        getPresentationState(q) === TaskPresentationStatus.PAUSED_WAITING) &&
      q.next_round_at &&
      q.next_round_at > 0 &&
      q.interval_minute &&
      q.interval_minute > 0,
  );
  if (hasIntervalWaiting) {
    startCountdownTicker();
  }
}

function bindTaskQueueDragEvents(container: HTMLElement) {
  if (!container) return;
  _queueSortable?.destroy();
  _queueSortable = Sortable.create(container, {
    animation: 180,
    forceFallback: true,
    fallbackTolerance: 4,
    fallbackClass: 'task-queue-fallback',
    handle: '.task-queue-drag-handle',
    draggable: '.task-queue-item',
    ghostClass: 'task-queue-ghost',
    chosenClass: 'task-queue-chosen',
    dragClass: 'task-queue-dragging',
    onStart: () => {
      document.body.classList.add('task-queue-sorting');
    },
    onEnd: () => {
      const newOrder = [...container.querySelectorAll('.task-queue-item')]
        .map(el => (el as HTMLElement).dataset.taskId)
        .map(id => (id ? decodeURIComponent(id) : ''))
        .filter((id): id is string => Boolean(id));
      reorderGlobalQueue(newOrder);
      document.body.classList.remove('task-queue-sorting');
      loadChainForDevice('');
    },
  });
}
