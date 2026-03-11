import { TaskStatus } from './constants';
import { activeTask, globalQueue } from './state';
import { Task } from './types';
import { $, esc } from './utils';

/** 上一次渲染的任务快照，用于快速跳过无变化刷新 (#5) */
let _lastQueueHtml = '';

/* ===== Task Queue (Right Column) ===== */

export function loadChainForDevice(_serial: string) {
  const cards = $('#chain-cards')!;

  const queueSub = document.querySelector('.queue-sub');
  const executingCount = globalQueue.filter(t => t.status === TaskStatus.EXECUTING).length;
  if (queueSub)
    queueSub.textContent = `${executingCount} 个执行中 · 共 ${globalQueue.length} 个任务`;

  const newHtml = globalQueue
    .map((q: Task) => {
      const isActive = activeTask ? q.id === activeTask.id : false;
      const kwTotal = q.cities.reduce((s: number, c: { total: number }) => s + c.total, 0);
      const cityCount = q.cities.length;
      const safeId = esc(q.id); // #8: XSS 转义
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
        <div class="flex gap-4 text-s500">
          <div class="flex items-center gap-1">
            <span class="material-symbols-outlined icon-xs text-[14px]">domain</span>
            <span class="text-[10px] font-medium">${cityCount} 城市</span>
          </div>
          <div class="flex items-center gap-1">
            <span class="material-symbols-outlined icon-xs text-[14px]">sell</span>
            <span class="text-[10px] font-medium">${kwTotal} 关键词</span>
          </div>
        </div>`;

      if (q.status === TaskStatus.EXECUTING) {
        const kwDone = q.cities.reduce((s: number, c: { done: number }) => s + c.done, 0);
        const pct = kwTotal > 0 ? Math.round((kwDone / kwTotal) * 100) : 0;
        return `
      <div class="bg-blue-50/40 border border-blue-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all hover:shadow-md ${ring}" onclick="window.__switchTask('${safeId}')">
        <div class="w-1 self-stretch bg-[#2563EB]"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-xs text-s700 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="flex items-center gap-1.5 bg-s100 px-2 py-0.5 rounded-full shrink-0">
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
        const kwDone = q.cities.reduce((s: number, c: { done: number }) => s + c.done, 0);
        const pct = kwTotal > 0 ? Math.round((kwDone / kwTotal) * 100) : 0;
        return `
      <div class="bg-amber-50/40 border border-amber-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all hover:shadow-md ${ring}" onclick="window.__switchTask('${safeId}')">
        <div class="w-1 self-stretch bg-amber-400"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-xs text-s700 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="bg-s100 px-2 py-0.5 rounded-full shrink-0">
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
      <div class="bg-s50/60 border border-s200 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all hover:shadow-md ${ring}" onclick="window.__switchTask('${safeId}')">
        <div class="w-1 self-stretch bg-[#64748B]"></div>
        <div class="flex-1 p-3">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-xs text-s700 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="bg-s100 px-2 py-0.5 rounded-full shrink-0">
              <span class="text-[11px] font-bold text-[#64748B]">等待中</span>
            </div>
          </div>
          ${statsRow}
        </div>
      </div>`;
      } else if (q.status === TaskStatus.SUCCESS) {
        return `
      <div class="bg-green-50/40 border border-green-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer opacity-80 transition-all hover:opacity-100 ${ring}" onclick="window.__switchTask('${safeId}')">
        <div class="w-1 self-stretch bg-[#10B981]"></div>
        <div class="flex-1 p-3">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-xs text-s700 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="bg-s100 px-2 py-0.5 rounded-full shrink-0">
              <span class="text-[11px] font-bold text-[#10B981]">已完成</span>
            </div>
          </div>
          ${statsRow}
        </div>
      </div>`;
      } else if (q.status === TaskStatus.ERROR) {
        const kwDone = q.cities.reduce((s: number, c: { done: number }) => s + c.done, 0);
        const pct = kwTotal > 0 ? Math.round((kwDone / kwTotal) * 100) : 0;
        return `
      <div class="bg-red-50/40 border border-red-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all hover:shadow-md ${ring}" onclick="window.__switchTask('${safeId}')">
        <div class="w-1 self-stretch bg-red-500"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-xs text-s700 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="bg-s100 px-2 py-0.5 rounded-full shrink-0">
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
}
