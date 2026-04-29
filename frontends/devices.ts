import { invoke } from '@tauri-apps/api/core';

import { DeviceState, TaskPresentationStatus } from './constants';
import { getAssignedDeviceSerials, globalQueue, selectedDevice, setSelectedDevice } from './state';
import { DeviceRow } from './types';
import { $, esc, getDeviceName, getPresentationState, timeAgo } from './utils';

// 回调注册（由 main.ts 初始化后设置，避免循环依赖）
let _onLoadTasksForDevice: ((serial: string) => void) | null = null;
let _onShowDeviceInfo: ((serial: string) => void) | null = null;
let _onStartMirror: ((serial: string) => void) | null = null;
let _onSwitchToWifi: ((serial: string) => void | Promise<void>) | null = null;
let _onDisconnectWifi: ((serial: string) => void | Promise<void>) | null = null;
let _pendingRafId: number | null = null;
let _deviceCache: DeviceRow[] = [];
// 正在切到无线的 serial 集合：作为 spinner 状态的唯一来源，
// 让 monitor 触发的中间 re-render 不会把 progress_activity 旋转抹掉
const _switchingSerials = new Set<string>();
// 委托监听只挂一次到 #device-tree，避免 re-render 累积监听器
let _treeListenerAttached = false;

export function rerenderDeviceCardsFromCache() {
  if (_deviceCache.length > 0) {
    renderDeviceCards(_deviceCache);
  }
}

/** MEM-L4: beforeunload 时由 main.ts 调用，取消挂起的 rAF，防止页面卸载后仍执行 DOM 更新 */
export function cleanupDevices() {
  if (_pendingRafId) {
    cancelAnimationFrame(_pendingRafId);
    _pendingRafId = null;
  }
}

export function setDeviceCallbacks(
  onLoadTasks: (serial: string) => void,
  onShowDeviceInfo: (serial: string) => void,
  onStartMirror?: (serial: string) => void,
  onSwitchToWifi?: (serial: string) => void | Promise<void>,
  onDisconnectWifi?: (serial: string) => void | Promise<void>,
) {
  _onLoadTasksForDevice = onLoadTasks;
  _onShowDeviceInfo = onShowDeviceInfo;
  _onStartMirror = onStartMirror ?? null;
  _onSwitchToWifi = onSwitchToWifi ?? null;
  _onDisconnectWifi = onDisconnectWifi ?? null;
}

/* ===== Device List ===== */

export async function refreshDevices(): Promise<DeviceRow[]> {
  const tree = $('#device-tree')!;

  try {
    const devs: DeviceRow[] = await invoke('list_devices');
    _deviceCache = devs;
    const executingSerials = getAssignedDeviceSerials();
    let clearedSelection = false;

    if (selectedDevice) {
      const selectedRow = devs.find(d => d.serial === selectedDevice);
      const shouldClearSelection =
        !selectedRow ||
        (selectedRow.state !== DeviceState.OFFLINE &&
          !selectedRow.is_flagged &&
          !executingSerials.has(selectedRow.serial));

      if (shouldClearSelection) {
        setSelectedDevice(null);
        clearedSelection = true;
        const removeBtn = $('#btn-remove-selected') as HTMLButtonElement | null;
        const unflagBtn = $('#btn-unflag-device') as HTMLButtonElement | null;
        if (removeBtn) removeBtn.disabled = true;
        if (unflagBtn) unflagBtn.disabled = true;
      }
    }

    renderDeviceCards(devs);
    if (clearedSelection) {
      updateCardSelection();
    }

    return devs;
  } catch (e) {
    _deviceCache = [];
    tree.innerHTML = `<div class="empty-hint text-red">错误: ${e}</div>`;
    return [];
  }
}

export function getCachedDevice(serial: string): DeviceRow | undefined {
  return _deviceCache.find(item => item.serial === serial);
}

export function getCachedDeviceResolution(
  serial: string,
): { width: number; height: number } | null {
  const device = _deviceCache.find(item => item.serial === serial);
  const raw = device?.display_resolution?.trim();
  if (!raw) return null;

  const match = raw.match(/(\d+)\s*[x×]\s*(\d+)/i);
  if (!match) return null;

  const width = Number.parseInt(match[1], 10);
  const height = Number.parseInt(match[2], 10);
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    return null;
  }

  return { width, height };
}

interface TransportCtx {
  running: boolean;
  flagged: boolean;
  hasError: boolean;
  offline: boolean;
}

/**
 * 左侧 transport 图标按钮：兼任三件事 ——
 * 1. 视觉传达 transport（USB / WiFi 用配色 + 角标区分）
 * 2. ready USB 卡上点击触发切到无线（其他状态 disabled）
 * 3. 切换中由事件 handler 切换为 progress_activity 旋转
 */
function transportIconButton(d: DeviceRow, ctx: TransportCtx): string {
  const isWifi = d.device_type === 'wifi';
  const switching = _switchingSerials.has(d.serial);
  // USB 状态下：仅 ready 可点（切到无线）
  // WiFi 状态下：在线即可点（断开无线）
  const usbClickable =
    !isWifi && !ctx.running && !ctx.flagged && !ctx.hasError && !ctx.offline && !switching;
  const wifiClickable = isWifi && !ctx.offline && !switching;

  let frameCls: string;
  if (ctx.offline) {
    frameCls = 'bg-s200 border-s300 text-s400';
  } else if (ctx.hasError) {
    frameCls = 'bg-red-50 border-red-200 text-red-400';
  } else if (ctx.flagged) {
    frameCls = 'bg-orange-50 border-orange-200 text-orange-400';
  } else if (isWifi) {
    frameCls = 'bg-blue-50 border-blue-200 text-blue-600';
  } else if (ctx.running) {
    frameCls = 'bg-white border-blue-100 text-blue-500';
  } else {
    frameCls = 'bg-white border-blue-100 text-blue-500';
  }

  let title: string;
  if (switching) {
    title = isWifi ? '断开中…' : '切换中…';
  } else if (isWifi) {
    title = ctx.offline ? '设备离线' : '已连接无线 — 点击断开';
  } else if (ctx.offline) {
    title = '设备离线';
  } else if (ctx.hasError) {
    title = '任务异常，不可切换';
  } else if (ctx.flagged) {
    title = '设备已风控，不可切换';
  } else if (ctx.running) {
    title = '任务执行中，无法切换';
  } else {
    title = '切到无线';
  }

  // switching 状态由 _switchingSerials Set 驱动，跨 re-render 保持
  let actionAttr: string;
  if (usbClickable) {
    actionAttr = `data-transport-switch="${esc(d.serial)}"`;
  } else if (wifiClickable) {
    actionAttr = `data-transport-disconnect="${esc(d.serial)}"`;
  } else {
    actionAttr = 'disabled';
  }
  const loadingCls = switching ? ' is-loading' : '';
  const mainIcon = switching ? 'progress_activity' : 'smartphone';
  const wifiBadge =
    isWifi && !switching
      ? '<span class="dev-transport-wifi-badge material-symbols-outlined">wifi</span>'
      : '';

  return `<button type="button" class="dev-transport-btn${loadingCls} h-9 w-9 rounded-md ${frameCls} border flex items-center justify-center shrink-0 relative" title="${esc(title)}" ${actionAttr}>
      <span class="dev-transport-main material-symbols-outlined text-xl fill-1">${mainIcon}</span>
      ${wifiBadge}
    </button>`;
}

function renderDeviceCards(devs: DeviceRow[]) {
  const tree = $('#device-tree')!;
  const devCount = $('#dev-count');
  if (devCount) devCount.textContent = `共 ${devs.length} 台`;

  const executingSerials = getAssignedDeviceSerials();

  const runningDevs: DeviceRow[] = [];
  const readyDevs: DeviceRow[] = [];
  const offlineDevs: DeviceRow[] = [];

  devs.forEach(d => {
    if (d.state === DeviceState.OFFLINE) {
      offlineDevs.push(d);
    } else if (d.is_flagged) {
      // flagged 设备始终归入就绪区域（显示风控样式）
      // 即使 globalQueue 中还未清除 assigned_device（竞态防护）
      readyDevs.push(d);
    } else if (executingSerials.has(d.serial)) {
      runningDevs.push(d);
    } else {
      readyDevs.push(d);
    }
  });

  const renderRunningCard = (d: DeviceRow) => {
    const displayName = getDeviceName(d);
    const battery = d.battery_level;
    const temp = d.battery_temperature;
    const hwid = d.hw_serial || d.serial;
    const shortHwid = hwid.length > 16 ? hwid.substring(0, 16) + '…' : hwid;
    const devTask = globalQueue.find(t => t.assigned_device === d.serial);
    const taskLabel = devTask ? devTask.name : '空闲';
    const progress = devTask?.progress ?? 0;
    const presentation = devTask ? getPresentationState(devTask) : TaskPresentationStatus.RUNNING;
    const statusLabel =
      presentation === TaskPresentationStatus.WAITING_NEXT_ROUND
        ? '等待中'
        : presentation === TaskPresentationStatus.ERROR_PAUSED
          ? '任务异常'
          : '执行中';
    const statusTextClass =
      presentation === TaskPresentationStatus.WAITING_NEXT_ROUND
        ? 'text-violet-600'
        : presentation === TaskPresentationStatus.ERROR_PAUSED
          ? 'text-red-600'
          : 'text-blue-600';
    const statusDotClass =
      presentation === TaskPresentationStatus.WAITING_NEXT_ROUND
        ? 'bg-violet-500'
        : presentation === TaskPresentationStatus.ERROR_PAUSED
          ? 'bg-red-500 animate-pulse'
          : 'bg-blue-500';
    const progressLabel =
      devTask && devTask.keyword_total > 0
        ? `${devTask.keyword_done}/${devTask.keyword_total}`
        : taskLabel;
    const batteryIcon =
      battery > 80 ? 'battery_charging_80' : battery > 50 ? 'battery_5_bar' : 'battery_3_bar';
    const isSel = d.serial === selectedDevice;

    return `<div class="dev-card device-card-running border rounded-md p-2.5 transition-all hover:shadow-sm cursor-pointer ${isSel ? 'ring-2 ring-blue-300' : ''}" data-s="${esc(d.serial)}" data-state="running">
      <div class="flex items-start gap-3">
        ${transportIconButton(d, { running: true, flagged: false, hasError: false, offline: false })}
        <div class="min-w-0 flex-1">
          <div class="flex justify-between items-center mb-0.5">
            <h3 class="text-[11px] font-bold text-s900 truncate">${esc(displayName)}</h3>
            <div class="flex items-center gap-1.5">
              <span class="w-1.5 h-1.5 rounded-full ${statusDotClass}"></span>
              <span class="text-[10px] font-bold ${statusTextClass}">${statusLabel}</span>
            </div>
          </div>
          <p class="mono-technical text-[10px] text-s500 font-medium truncate">${esc(shortHwid)}</p>
        </div>
      </div>
      <div class="mt-2.5">
        <div class="flex justify-between items-end mb-1">
          <span class="text-[10px] font-bold text-blue-600 uppercase tracking-tight">${esc(progressLabel)}</span>
          <span class="text-[10px] font-black text-blue-600">${progress}%</span>
        </div>
        <div class="w-full h-1 bg-white/80 rounded-full overflow-hidden">
          <div class="h-full bg-blue-500 rounded-full" style="width: ${progress}%"></div>
        </div>
      </div>
      <div class="mt-2.5 flex items-center justify-between text-[10px] font-bold">
        <div class="flex items-center gap-3">
          <span class="flex items-center gap-1 text-blue-600">
            <span class="material-symbols-outlined icon-sm fill-1">${batteryIcon}</span>${battery}%
          </span>
          <span class="flex items-center gap-1 text-s500">
            <span class="material-symbols-outlined icon-sm">device_thermostat</span>${temp}°C
          </span>
        </div>
        <button class="dev-mirror-btn" data-mirror="${esc(d.serial)}" title="投屏观察">
          <span class="material-symbols-outlined">screen_share</span>
        </button>
      </div>
    </div>`;
  };

  const renderReadyCard = (d: DeviceRow) => {
    const displayName = getDeviceName(d);
    const battery = d.battery_level;
    const temp = d.battery_temperature;
    const hwid = d.hw_serial || d.serial;
    const shortHwid = hwid.length > 16 ? hwid.substring(0, 16) + '…' : hwid;
    const batteryIcon =
      battery > 80 ? 'battery_full' : battery > 50 ? 'battery_5_bar' : 'battery_3_bar';
    const batteryColor =
      battery > 80 ? 'text-green-600' : battery > 30 ? 'text-blue-600' : 'text-orange-500';
    const tempColor = temp > 40 ? 'text-orange-500' : 'text-s500';
    const isSel = d.serial === selectedDevice;
    const flagged = d.is_flagged;

    // 检查是否有 ERROR_PAUSED 任务绑定了这台设备（致命错误后释放但保留 serial）
    // 注意：FatalError 会同时 flag 设备，此处 hasError 不排除 flagged 情况
    const errorTask = globalQueue.find(
      t =>
        t.assigned_device === d.serial &&
        getPresentationState(t) === TaskPresentationStatus.ERROR_PAUSED,
    );
    const hasError = !!errorTask;

    const badgeCls = hasError
      ? 'bg-red-50 text-red-600 border-red-200'
      : flagged
        ? 'bg-orange-50 text-orange-600 border-orange-200'
        : 'bg-blue-50 text-blue-600 border-blue-100';
    const badgeText = hasError ? '任务异常' : flagged ? '风控' : '就绪';
    const opacityCls = flagged && !hasError ? 'opacity-70' : '';
    const ringCls = isSel
      ? hasError
        ? 'ring-2 ring-red-200'
        : flagged
          ? 'ring-2 ring-orange-300'
          : 'ring-2 ring-blue-200'
      : '';
    const errorBanner =
      hasError && errorTask
        ? `<div class="mt-2 flex items-center gap-1.5 px-2 py-1 bg-red-50 rounded border border-red-100">
          <span class="material-symbols-outlined text-red-400" style="font-size:12px">error</span>
          <span class="text-[10px] text-red-600 font-semibold truncate">${esc(errorTask.name)}</span>
        </div>`
        : '';

    return `<div class="dev-card device-card-ready border rounded-md p-2.5 transition-all hover:border-blue-200 cursor-pointer relative ${opacityCls} ${ringCls}" data-s="${esc(d.serial)}" data-state="ready" data-flagged="${flagged ? '1' : '0'}" data-error="${hasError ? '1' : '0'}">
      <div class="absolute top-2.5 right-2.5 px-1.5 py-0.5 ${badgeCls} text-[10px] font-black rounded border uppercase tracking-normal">${badgeText}</div>
      <div class="flex items-start gap-3">
        ${transportIconButton(d, { running: false, flagged, hasError, offline: false })}
        <div class="min-w-0 flex-1">
          <h3 class="text-[11px] font-bold text-s700 truncate pr-8">${esc(displayName)}</h3>
          <p class="mono-technical text-[10px] text-s500 mt-0.5 font-medium truncate pr-8">${esc(shortHwid)}</p>
        </div>
      </div>
      <div class="mt-2.5 flex items-center justify-between text-[10px] font-bold">
        <div class="flex items-center gap-3">
          <span class="flex items-center gap-1 ${batteryColor}">
            <span class="material-symbols-outlined icon-sm fill-1">${batteryIcon}</span>${battery}%
          </span>
          <span class="flex items-center gap-1 ${tempColor}">
            <span class="material-symbols-outlined icon-sm">device_thermostat</span>${temp}°C
          </span>
        </div>
        <div class="flex items-center gap-1">
          <button class="dev-mirror-btn" data-mirror="${esc(d.serial)}" title="投屏">
            <span class="material-symbols-outlined">screen_share</span>
          </button>
        </div>
      </div>
      ${errorBanner}
    </div>`;
  };

  const renderOfflineCard = (d: DeviceRow) => {
    const displayName = getDeviceName(d);
    const battery = d.battery_level;
    const temp = d.battery_temperature;
    const hwid = d.hw_serial || d.serial;
    const shortHwid = hwid.length > 16 ? hwid.substring(0, 16) + '…' : hwid;
    const isSel = d.serial === selectedDevice;

    return `<div class="dev-card device-card-offline border rounded-md p-2.5 cursor-pointer relative ${isSel ? 'ring-2 ring-s400' : ''}" data-s="${esc(d.serial)}" data-state="offline">
      <div class="absolute top-2.5 right-2.5 px-1.5 py-0.5 bg-s200 text-s500 text-[10px] font-black rounded border border-s300 uppercase tracking-normal">离线</div>
      <div class="flex items-start gap-3">
        ${transportIconButton(d, { running: false, flagged: false, hasError: false, offline: true })}
        <div class="min-w-0 flex-1">
          <h3 class="text-[11px] font-bold text-s600 truncate pr-8">${esc(displayName)}</h3>
          <p class="mono-technical text-[10px] text-s400 mt-0.5 font-medium truncate pr-8">${esc(shortHwid)}</p>
        </div>
      </div>
      <div class="mt-2.5 flex items-center justify-between text-[10px] font-bold">
        <div class="flex gap-3">
          <span class="flex items-center gap-1 text-s400">
            <span class="material-symbols-outlined icon-sm">battery_3_bar</span>${battery}%
          </span>
          <span class="flex items-center gap-1 text-s400">
            <span class="material-symbols-outlined icon-sm">device_thermostat</span>${temp}°C
          </span>
        </div>
        <span class="text-s400 uppercase text-[10px]">${timeAgo(d.updated_at)}</span>
      </div>
    </div>`;
  };

  const emptyBody =
    '<div class="text-center text-[9px] text-s300 py-3 font-semibold uppercase tracking-wider">暂无设备</div>';
  let html = '';

  html += `<div class="dev-section${runningDevs.length ? '' : ' collapsed'}">
      <div class="dev-section-header bg-blue-50 px-3 py-1.5 flex items-center gap-2 sticky top-0 z-10 border-b border-blue-100" onclick="this.parentElement.classList.toggle('collapsed')">
        <span class="material-symbols-outlined section-arrow text-sm text-blue-400">expand_more</span>
        <span class="text-[11px] font-black text-s500 uppercase tracking-normal">运行中</span>
      </div>
      <div class="dev-section-body p-2 space-y-2">${runningDevs.length ? runningDevs.map(renderRunningCard).join('') : emptyBody}</div>
    </div>`;

  html += `<div class="dev-section${readyDevs.length ? '' : ' collapsed'}">
      <div class="dev-section-header bg-s50 px-3 py-1.5 flex items-center gap-2 sticky top-0 z-10 border-b border-s100 mt-1" onclick="this.parentElement.classList.toggle('collapsed')">
        <span class="material-symbols-outlined section-arrow text-sm text-s400">expand_more</span>
        <span class="text-[11px] font-black text-s500 uppercase tracking-normal">就绪</span>
      </div>
      <div class="dev-section-body p-2 space-y-2">${readyDevs.length ? readyDevs.map(renderReadyCard).join('') : emptyBody}</div>
    </div>`;

  html += `<div class="dev-section${offlineDevs.length ? '' : ' collapsed'}">
      <div class="dev-section-header bg-s50 px-3 py-1.5 flex items-center gap-2 sticky top-0 z-10 border-b border-s100 mt-1" onclick="this.parentElement.classList.toggle('collapsed')">
        <span class="material-symbols-outlined section-arrow text-sm text-s400">expand_more</span>
        <span class="text-[11px] font-black text-s500 uppercase tracking-normal">离线</span>
      </div>
      <div class="dev-section-body p-2 space-y-2">${offlineDevs.length ? offlineDevs.map(renderOfflineCard).join('') : emptyBody}</div>
    </div>`;

  // 使用 rAF 避免在 JS 主线程繁忙时强制同步重排
  // 取消上一次挂起的 rAF，避免竞态（task://update 和 devices-changed 几乎同时触发时）
  if (_pendingRafId) {
    cancelAnimationFrame(_pendingRafId);
    _pendingRafId = null;
  }

  _pendingRafId = requestAnimationFrame(() => {
    _pendingRafId = null;

    // 在 rAF 回调内采集折叠状态（此时读取的是最新 DOM，避免竞态）
    const sectionState = new Map<string, { collapsed: boolean; hadDevices: boolean }>();
    tree.querySelectorAll('.dev-section').forEach(el => {
      const label = el.querySelector('.dev-section-header span:last-child')?.textContent?.trim();
      if (!label) return;
      const body = el.querySelector('.dev-section-body');
      const hadDevices = body ? body.querySelectorAll('.dev-card').length > 0 : false;
      sectionState.set(label, { collapsed: el.classList.contains('collapsed'), hadDevices });
    });

    // innerHTML 去重：在 rAF 内部对比，确保比较的是当前真实 DOM
    if (tree.innerHTML === html) return;

    tree.innerHTML = html;

    tree.querySelectorAll('.dev-section').forEach(el => {
      const label = el.querySelector('.dev-section-header span:last-child')?.textContent?.trim();
      if (!label) return;
      const prev = sectionState.get(label);
      if (!prev) return;
      if (prev.collapsed && prev.hadDevices) {
        el.classList.add('collapsed');
      } else if (!prev.collapsed) {
        el.classList.remove('collapsed');
      }
    });

    attachDelegatedListeners(tree);
  });
}

/**
 * 单次挂载到 #device-tree 的事件委托：用 closest() 路由所有卡片交互。
 * 每次 re-render 替换 innerHTML 后旧节点连同其内联监听一起被 GC，
 * 这里挂在容器上的监听则跨 re-render 持续存活，避免累积/双触发。
 */
function attachDelegatedListeners(tree: HTMLElement) {
  if (_treeListenerAttached) return;
  _treeListenerAttached = true;

  tree.addEventListener('click', e => {
    const target = e.target as HTMLElement | null;
    if (!target) return;

    // 1) transport 按钮（USB→WiFi 切换）—— 优先于 mirror / card 路由
    const transportBtn = target.closest<HTMLButtonElement>(
      '.dev-transport-btn[data-transport-switch]',
    );
    if (transportBtn) {
      e.stopPropagation();
      const serial = transportBtn.dataset.transportSwitch;
      if (!serial || transportBtn.disabled || !_onSwitchToWifi) return;
      void handleSwitchToWifi(serial);
      return;
    }

    // 1b) transport 按钮（WiFi → 主动断开）
    const disconnectBtn = target.closest<HTMLButtonElement>(
      '.dev-transport-btn[data-transport-disconnect]',
    );
    if (disconnectBtn) {
      e.stopPropagation();
      const serial = disconnectBtn.dataset.transportDisconnect;
      if (!serial || disconnectBtn.disabled || !_onDisconnectWifi) return;
      void handleDisconnectWifi(serial);
      return;
    }

    // 2) 投屏按钮
    const mirrorBtn = target.closest<HTMLElement>('.dev-mirror-btn');
    if (mirrorBtn) {
      e.stopPropagation();
      const serial = mirrorBtn.dataset.mirror;
      if (serial) _onStartMirror?.(serial);
      return;
    }

    // 3) 卡片选中（点击空白区域）
    const card = target.closest<HTMLElement>('.dev-card');
    if (card) {
      const s = card.dataset.s;
      const state = card.dataset.state;
      if (s && (state === 'running' || state === 'offline' || state === 'ready')) {
        e.stopPropagation();
        selectDevice(s);
      }
    }
  });

  tree.addEventListener('dblclick', e => {
    const target = e.target as HTMLElement | null;
    const card = target?.closest<HTMLElement>('.dev-card');
    if (!card) return;
    e.stopPropagation();
    const s = card.dataset.s;
    if (s) _onShowDeviceInfo?.(s);
  });
}

async function handleSwitchToWifi(serial: string) {
  if (_switchingSerials.has(serial) || !_onSwitchToWifi) return;
  _switchingSerials.add(serial);
  // 立即重渲染让 spinner 来源于 Set，跨后续 monitor re-render 都能保持
  rerenderDeviceCardsFromCache();
  try {
    await _onSwitchToWifi(serial);
  } finally {
    _switchingSerials.delete(serial);
    // 命令成功通常会触发 devices-changed → refreshDevices；
    // 失败时这一步用缓存重绘把 spinner 状态清回正常，避免假死
    rerenderDeviceCardsFromCache();
  }
}

async function handleDisconnectWifi(serial: string) {
  if (_switchingSerials.has(serial) || !_onDisconnectWifi) return;
  _switchingSerials.add(serial);
  rerenderDeviceCardsFromCache();
  try {
    await _onDisconnectWifi(serial);
  } finally {
    _switchingSerials.delete(serial);
    rerenderDeviceCardsFromCache();
  }
}

export function filterDeviceCards(query: string) {
  const q = query.toLowerCase().trim();
  document.querySelectorAll('.dev-card').forEach(el => {
    const name = el.querySelector('h3')?.textContent?.toLowerCase() || '';
    const sub = el.querySelector('.mono-technical')?.textContent?.toLowerCase() || '';
    const match = !q || name.includes(q) || sub.includes(q);
    (el as HTMLElement).style.display = match ? '' : 'none';
  });
}

export function selectDevice(serial: string) {
  if (selectedDevice === serial) {
    setSelectedDevice(null);
    updateCardSelection();
    const removeBtn = $('#btn-remove-selected') as HTMLButtonElement;
    const unflagBtn = $('#btn-unflag-device') as HTMLButtonElement;
    if (removeBtn) removeBtn.disabled = true;
    if (unflagBtn) unflagBtn.disabled = true;
    return;
  }

  setSelectedDevice(serial);
  updateCardSelection();

  const removeBtn = $('#btn-remove-selected') as HTMLButtonElement;
  const unflagBtn = $('#btn-unflag-device') as HTMLButtonElement;
  const card = document.querySelector(`.dev-card[data-s="${serial}"]`) as HTMLElement | null;
  const isOffline = card?.dataset.state === 'offline';
  const isFlagged = card?.dataset.flagged === '1';
  const isError = card?.dataset.error === '1';
  if (removeBtn) {
    removeBtn.disabled = !isOffline;
  }
  if (unflagBtn) {
    // 风控标记 或 任务异常标记 都可以解除
    unflagBtn.disabled = !(isFlagged || isError);
  }

  // 有任务异常时仍加载任务（即使设备被 flag），纯风控设备不加载
  if (!isOffline && (!isFlagged || isError)) {
    _onLoadTasksForDevice?.(serial);
  }
}

/** 清除设备异常标记（将 ERROR_PAUSED 任务的 assigned_device 解除关联） */
export function clearDeviceErrorMark(serial: string) {
  // 找到绑定该设备的 ERROR_PAUSED 任务，切换到该任务让用户操作
  _onLoadTasksForDevice?.(serial);
}

export function updateCardSelection() {
  document.querySelectorAll('.dev-card').forEach(el => {
    const s = (el as HTMLElement).dataset.s;
    const state = (el as HTMLElement).dataset.state;
    const flagged = (el as HTMLElement).dataset.flagged === '1';
    el.classList.remove(
      'ring-2',
      'ring-1',
      'ring-blue-300',
      'ring-blue-200',
      'ring-orange-300',
      'ring-s400',
    );
    if (s === selectedDevice) {
      const isErr = (el as HTMLElement).dataset.error === '1';
      if (state === 'offline') {
        el.classList.add('ring-2', 'ring-s400');
      } else if (state === 'ready' && isErr) {
        el.classList.add('ring-2', 'ring-red-200');
      } else if (state === 'ready' && flagged) {
        el.classList.add('ring-2', 'ring-orange-300');
      } else if (state === 'ready') {
        el.classList.add('ring-2', 'ring-blue-200');
      } else {
        el.classList.add('ring-2', 'ring-blue-300');
      }
    }
  });
}
