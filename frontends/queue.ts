import Sortable from 'sortablejs';

import { TaskStatus } from './constants';
import { activeTask, globalQueue, reorderGlobalQueue } from './state';
import { TaskSummary } from './types';
import { $, esc } from './utils';

/** 上一次渲染的任务快照，用于快速跳过无变化刷新 (#5) */
let _lastQueueHtml = '';
let _queueSortable: Sortable | null = null;

/* ===== Task Queue (Right Column) ===== */

export function loadChainForDevice(_serial: string) {
  const cards = $('#chain-cards')!;

  const queueSub = document.querySelector('.queue-sub');
  const executingCount = globalQueue.filter(t => t.status === TaskStatus.EXECUTING).length;
  if (queueSub)
    queueSub.textContent = `${executingCount} 个执行中 · 共 ${globalQueue.length} 个任务`;

  const newHtml = globalQueue
    .map((q: TaskSummary) => {
      const isActive = activeTask ? q.id === activeTask.id : false;
      const kwTotal = q.keyword_total;
      const cityCount = q.city_count;
      const taskIdAttr = encodeURIComponent(q.id);
      const clickTaskId = JSON.stringify(q.id);
      const ringColor =
        q.status === TaskStatus.ERROR
          ? 'ring-red-200'
          : q.status === TaskStatus.PAUSED
            ? 'ring-amber-200'
            : q.status === TaskStatus.SUCCESS
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

      if (q.status === TaskStatus.EXECUTING) {
        const pct = q.progress;
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
            <span class="text-[10px] font-semibold text-s400">当前进度</span>
            <span class="text-[10px] font-bold text-[#2563EB]">${pct}%</span>
          </div>
          <div class="absolute bottom-0 left-1 right-0 h-[2px] bg-s100 overflow-hidden">
            <div class="h-full bg-[#2563EB]" style="width: ${pct}%"></div>
          </div>
        </div>
      </div>`;
      } else if (q.status === TaskStatus.PAUSED) {
        const pct = q.progress;
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
            <span class="text-[10px] font-semibold text-s400">暂停进度</span>
            <span class="text-[10px] font-bold text-amber-600">${pct}%</span>
          </div>
          <div class="absolute bottom-0 left-1 right-0 h-[2px] bg-s100 overflow-hidden">
            <div class="h-full bg-amber-400" style="width: ${pct}%"></div>
          </div>
        </div>
      </div>`;
      } else if (q.status === TaskStatus.WAITING) {
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
              <span class="text-[11px] font-bold text-[#64748B]">等待中</span>
            </div>
          </div>
          ${statsRow}
        </div>
      </div>`;
      } else if (q.status === TaskStatus.SUCCESS) {
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
      } else if (q.status === TaskStatus.ERROR) {
        const pct = q.progress;
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
            <span class="text-[10px] font-semibold text-s400">暂停进度</span>
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
