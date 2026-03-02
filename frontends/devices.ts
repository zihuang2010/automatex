import { invoke } from '@tauri-apps/api/core';
import { DeviceState } from './constants';
import { DeviceRow } from './types';
import { selectedDevice, globalQueue, setSelectedDevice, getAssignedDeviceSerials } from './state';
import { $, esc, timeAgo, getDeviceName } from './utils';

// 回调注册（由 main.ts 初始化后设置，避免循环依赖）
let _onLoadTasksForDevice: ((serial: string) => void) | null = null;
let _onShowDeviceInfo: ((serial: string) => void) | null = null;

export function setDeviceCallbacks(
  onLoadTasks: (serial: string) => void,
  onShowDeviceInfo: (serial: string) => void,
) {
  _onLoadTasksForDevice = onLoadTasks;
  _onShowDeviceInfo = onShowDeviceInfo;
}

/* ===== Device List ===== */

export async function refreshDevices(): Promise<DeviceRow[]> {
  const tree = $('#device-tree')!;

  try {
    const devs: DeviceRow[] = await invoke('list_devices');
    renderDeviceCards(devs);

    if (selectedDevice && !devs.some(d => d.serial === selectedDevice)) {
      setSelectedDevice(null);
    }

    return devs;
  } catch (e) {
    tree.innerHTML = `<div class="empty-hint text-red">错误: ${e}</div>`;
    return [];
  }
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
    const kwTotal =
      devTask?.cities.reduce((s: number, c: { total: number }) => s + c.total, 0) ?? 0;
    const kwDone = devTask?.cities.reduce((s: number, c: { done: number }) => s + c.done, 0) ?? 0;
    const progress = kwTotal > 0 ? Math.round((kwDone / kwTotal) * 100) : 0;
    const batteryIcon =
      battery > 80 ? 'battery_charging_80' : battery > 50 ? 'battery_5_bar' : 'battery_3_bar';
    const isSel = d.serial === selectedDevice;

    return `<div class="dev-card device-card-running border rounded-md p-2.5 transition-all hover:shadow-sm cursor-pointer ${isSel ? 'ring-2 ring-blue-300' : ''}" data-s="${esc(d.serial)}" data-state="running">
      <div class="flex items-start gap-3">
        <div class="h-9 w-9 rounded-lg bg-white border border-blue-100 flex items-center justify-center text-blue-500 shrink-0">
          <span class="material-symbols-outlined text-xl fill-1">smartphone</span>
        </div>
        <div class="min-w-0 flex-1">
          <div class="flex justify-between items-center mb-0.5">
            <h3 class="text-[11px] font-bold text-s900 truncate">${esc(displayName)}</h3>
            <div class="pulsing-dot scale-90"></div>
          </div>
          <p class="mono-technical text-[10px] text-s500 font-medium">${esc(shortHwid)}</p>
        </div>
      </div>
      <div class="mt-2.5">
        <div class="flex justify-between items-end mb-1">
          <span class="text-[10px] font-bold text-blue-600 uppercase tracking-tight">${esc(taskLabel)}</span>
          <span class="text-[10px] font-black text-blue-600">${progress}%</span>
        </div>
        <div class="w-full h-1 bg-white/80 rounded-full overflow-hidden">
          <div class="h-full bg-blue-500 rounded-full" style="width: ${progress}%"></div>
        </div>
      </div>
      <div class="mt-2.5 flex items-center gap-3 text-[10px] font-bold">
        <span class="flex items-center gap-1 text-blue-600">
          <span class="material-symbols-outlined icon-sm fill-1">${batteryIcon}</span>${battery}%
        </span>
        <span class="flex items-center gap-1 text-s500">
          <span class="material-symbols-outlined icon-sm">device_thermostat</span>${temp}°C
        </span>
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

    const badgeCls = flagged
      ? 'bg-orange-50 text-orange-600 border-orange-200'
      : 'bg-blue-50 text-blue-600 border-blue-100';
    const badgeText = flagged ? '风控' : '就绪';
    const iconBorderCls = flagged
      ? 'bg-orange-50 border-orange-200 text-orange-400'
      : 'bg-s50 border-s100 text-s400';
    const opacityCls = flagged ? 'opacity-70' : '';
    const ringCls = isSel ? (flagged ? 'ring-2 ring-orange-300' : 'ring-2 ring-blue-200') : '';

    return `<div class="dev-card device-card-ready border rounded-md p-2.5 transition-all hover:border-blue-200 cursor-pointer relative ${opacityCls} ${ringCls}" data-s="${esc(d.serial)}" data-state="ready" data-flagged="${flagged ? '1' : '0'}">
      <div class="absolute top-2.5 right-2.5 px-1.5 py-0.5 ${badgeCls} text-[12px] font-black rounded border uppercase tracking-normal">${badgeText}</div>
      <div class="flex items-start gap-3">
        <div class="h-9 w-9 rounded-lg ${iconBorderCls} border flex items-center justify-center shrink-0">
          <span class="material-symbols-outlined text-xl fill-1">smartphone</span>
        </div>
        <div class="min-w-0 flex-1">
          <h3 class="text-[11px] font-bold text-s700 truncate pr-8">${esc(displayName)}</h3>
          <p class="mono-technical text-[10px] text-s500 mt-0.5 font-medium">${esc(shortHwid)}</p>
        </div>
      </div>
      <div class="mt-2.5 flex items-center gap-3 text-[10px] font-bold">
        <span class="flex items-center gap-1 ${batteryColor}">
          <span class="material-symbols-outlined icon-sm fill-1">${batteryIcon}</span>${battery}%
        </span>
        <span class="flex items-center gap-1 ${tempColor}">
          <span class="material-symbols-outlined icon-sm">device_thermostat</span>${temp}°C
        </span>
      </div>
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
        <div class="h-9 w-9 rounded-lg bg-s200 border border-s300 flex items-center justify-center text-s400 shrink-0">
          <span class="material-symbols-outlined text-xl fill-1">smartphone</span>
        </div>
        <div class="min-w-0 flex-1">
          <h3 class="text-[11px] font-bold text-s600 truncate pr-8">${esc(displayName)}</h3>
          <p class="mono-technical text-[10px] text-s400 mt-0.5 font-medium">${esc(shortHwid)}</p>
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

  const sectionState = new Map<string, { collapsed: boolean; hadDevices: boolean }>();
  tree.querySelectorAll('.dev-section').forEach(el => {
    const label = el.querySelector('.dev-section-header span:last-child')?.textContent?.trim();
    if (!label) return;
    const body = el.querySelector('.dev-section-body');
    const hadDevices = body ? body.querySelectorAll('.dev-card').length > 0 : false;
    sectionState.set(label, { collapsed: el.classList.contains('collapsed'), hadDevices });
  });

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

  tree.querySelectorAll('.dev-card').forEach(el => {
    const s = (el as HTMLElement).dataset.s!;
    const state = (el as HTMLElement).dataset.state;

    const flagged = (el as HTMLElement).dataset.flagged === '1';

    if (state === 'running' || state === 'offline' || (state === 'ready' && flagged)) {
      el.addEventListener('click', e => {
        e.stopPropagation();
        selectDevice(s);
      });
    }

    el.addEventListener('dblclick', e => {
      e.stopPropagation();
      _onShowDeviceInfo?.(s);
    });
  });
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
  if (removeBtn) {
    removeBtn.disabled = !isOffline;
  }
  if (unflagBtn) {
    unflagBtn.disabled = !isFlagged;
  }

  if (!isOffline && !isFlagged) {
    _onLoadTasksForDevice?.(serial);
  }
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
      if (state === 'offline') {
        el.classList.add('ring-2', 'ring-s400');
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
