import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
    type MockTask,
    getTasksForDevice,
    getAssignedDeviceSerials,
    releaseTasksForOfflineDevices,
    startTask,
    pauseTask,
    resumeTask,
    stopTask,
    retryTask,
    setTaskTickCallback,
    startTaskExecution,
    stopTaskExecution,
} from './mock-data';

/* ===== Types ===== */
/** 设备行（与后端 DeviceRow 一一对应） */
interface DeviceRow {
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
    updated_at: number;
}

/** 保留给 showDeviceInfo 弹窗使用 */
interface DeviceProperties {
    serial: string;
    model: string;
    brand: string;
    android_version: string;
    sdk_version: string;
    display_resolution: string;
    device_type: string;
    battery_level: number;
    battery_temperature: number;
}

let selectedDevice: string | null = null;
let globalQueue: MockTask[] = [];
let activeTask: MockTask | null = null;
let activeCityIdx = 0;

/** 将 unix 时间戳（秒）转为中文相对时间 */
function timeAgo(unixSec: number): string {
    const diff = Math.floor(Date.now() / 1000) - unixSec;
    if (diff < 60) return '刚刚';
    if (diff < 3600) return `${Math.floor(diff / 60)}分钟前`;
    if (diff < 86400) return `${Math.floor(diff / 3600)}小时前`;
    return `${Math.floor(diff / 86400)}天前`;
}

const $ = (s: string) => document.querySelector(s) as HTMLElement | null;

/** UI 内 Toast 提示（替代 alert，3 秒自动消失） */
function showToast(msg: string, type: 'info' | 'warning' | 'error' = 'warning') {
    const colorMap = {
        // 降低背景饱和度 (bg-opacity-80)，加入磨砂质感和白色高光边框 (border-white/50)
        info: 'bg-blue-50/80 backdrop-blur-md text-blue-700 border-white/50 shadow-md shadow-blue-900/5',
        warning:
            'bg-amber-50/80 backdrop-blur-md text-amber-800 border-white/50 shadow-md shadow-amber-900/5',
        error: 'bg-red-50/80 backdrop-blur-md text-red-700 border-white/50 shadow-md shadow-red-900/5',
    };

    const iconMap = {
        info: 'info',
        warning: 'warning',
        error: 'error',
    };

    const iconColorMap = {
        info: 'text-blue-500',
        warning: 'text-amber-600',
        error: 'text-red-500',
    };

    const toast = document.createElement('div');

    toast.className = `fixed top-6 left-1/2 -translate-x-1/2 z-[9999] px-6 py-3 rounded-full border ${colorMap[type]} text-xs font-medium shadow-md transition-all duration-300 opacity-0 -translate-y-4 flex items-center gap-2.5`;

    toast.innerHTML = `<span class="material-symbols-outlined text-base ${iconColorMap[type]}">${iconMap[type]}</span><span>${msg}</span>`;
    document.body.appendChild(toast);

    // 【动画优化】使用双重 requestAnimationFrame 确保浏览器正确渲染过渡动画
    requestAnimationFrame(() => {
        requestAnimationFrame(() => {
            // 移除透明和向上的位移，回归正常位置
            toast.classList.remove('opacity-0', '-translate-y-4');
            toast.classList.add('opacity-100', 'translate-y-0');
        });
    });

    // 3秒后退出动画
    setTimeout(() => {
        toast.classList.remove('opacity-100', 'translate-y-0');
        toast.classList.add('opacity-0', '-translate-y-4'); // 向上滑动并消失

        // 等待动画结束(300ms)后移除 DOM
        setTimeout(() => toast.remove(), 300);
    }, 3000);
}

/** 获取设备显示名称 */
function getDeviceName(d: DeviceRow): string {
    if (d.brand !== 'unknown' && d.model !== 'unknown') return `${d.brand} ${d.model}`;
    if (d.model !== 'unknown') return d.model;
    if (d.name && d.name !== d.serial) return d.name;
    return d.serial;
}

/* ───── Splash ───── */
function splash() {
    const el = $('#splash')!;
    const app = $('#app')!;
    // Show current time
    const timeEl = el.querySelector('.splash-time');
    if (timeEl) {
        const now = new Date();
        const h = String(now.getHours()).padStart(2, '0');
        const m = String(now.getMinutes()).padStart(2, '0');
        timeEl.textContent = `${h}:${m}`;
    }
    // Trigger enhanced entrance animation
    setTimeout(() => el.classList.add('loaded'), 300);
    setTimeout(() => {
        el.classList.add('out');
        app.classList.add('show');
        setTimeout(() => el.remove(), 800);
    }, 3200);
}

/* ───── Device List ───── */

async function refreshDevices() {
    const tree = $('#device-tree')!;

    try {
        const devs: DeviceRow[] = await invoke('list_devices');

        // Update dev count badge
        const devCount = $('#dev-count');
        if (devCount) devCount.textContent = `共 ${devs.length} 台`;

        renderDeviceCards(devs);

        // 检查当前选中设备是否仍在列表中
        if (selectedDevice && !devs.some(d => d.serial === selectedDevice)) {
            selectedDevice = null;
        }

        // 就绪设备不应有选中状态：若 selectedDevice 已无任务绑定则取消选中
        if (selectedDevice && !getAssignedDeviceSerials().has(selectedDevice)) {
            const selDev = devs.find(d => d.serial === selectedDevice);
            if (selDev && selDev.state !== 'Offline') {
                selectedDevice = null;
            }
        }
    } catch (e) {
        tree.innerHTML = `<div class="empty-hint text-red">错误: ${e}</div>`;
    }
}

function renderDeviceCards(devs: DeviceRow[]) {
    const tree = $('#device-tree')!;
    const devCount = $('#dev-count');
    if (devCount) devCount.textContent = `共 ${devs.length} 台`;

    // Split devices: Running / Ready / Offline
    const executingSerials = new Set(
        globalQueue
            .filter(t => t.status === 'EXECUTING' && t.assignedDevice)
            .map(t => t.assignedDevice!),
    );

    const runningDevs: DeviceRow[] = [];
    const readyDevs: DeviceRow[] = [];
    const offlineDevs: DeviceRow[] = [];

    devs.forEach(d => {
        if (d.state === 'Offline') {
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
        const devTask = globalQueue.find(t => t.assignedDevice === d.serial);
        const taskLabel = devTask ? devTask.name : '空闲';
        // 总体进度（所有城市的关键词完成比例）
        const kwTotal = devTask?.cities.reduce((s, c) => s + c.total, 0) ?? 0;
        const kwDone = devTask?.cities.reduce((s, c) => s + c.done, 0) ?? 0;
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
            <h3 class="text-[11px] font-bold text-slate-900 truncate">${esc(displayName)}</h3>
            <div class="pulsing-dot scale-90"></div>
          </div>
          <p class="mono-technical text-[10px] text-slate-500 font-medium">${esc(shortHwid)}</p>
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
        <span class="flex items-center gap-1 text-slate-500">
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
        const tempColor = temp > 40 ? 'text-orange-500' : 'text-slate-500';
        const isSel = d.serial === selectedDevice;

        return `<div class="dev-card device-card-ready border rounded-md p-2.5 transition-all hover:border-blue-200 cursor-pointer relative ${isSel ? 'ring-2 ring-blue-200' : ''}" data-s="${esc(d.serial)}" data-state="ready">
      <div class="absolute top-2.5 right-2.5 px-1.5 py-0.5 bg-blue-50 text-blue-600 text-[12px] font-black rounded border border-blue-100 uppercase tracking-normal">就绪</div>
      <div class="flex items-start gap-3">
        <div class="h-9 w-9 rounded-lg bg-slate-50 border border-slate-100 flex items-center justify-center text-slate-400 shrink-0">
          <span class="material-symbols-outlined text-xl fill-1">smartphone</span>
        </div>
        <div class="min-w-0 flex-1">
          <h3 class="text-[11px] font-bold text-slate-700 truncate pr-8">${esc(displayName)}</h3>
          <p class="mono-technical text-[10px] text-slate-500 mt-0.5 font-medium">${esc(shortHwid)}</p>
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

        return `<div class="dev-card device-card-offline border rounded-md p-2.5 cursor-pointer relative ${isSel ? 'ring-2 ring-slate-400' : ''}" data-s="${esc(d.serial)}" data-state="offline">
      <div class="absolute top-2.5 right-2.5 px-1.5 py-0.5 bg-slate-200 text-slate-500 text-[10px] font-black rounded border border-slate-300 uppercase tracking-normal">离线</div>
      <div class="flex items-start gap-3">
        <div class="h-9 w-9 rounded-lg bg-slate-200 border border-slate-300 flex items-center justify-center text-slate-400 shrink-0">
          <span class="material-symbols-outlined text-xl fill-1">smartphone</span>
        </div>
        <div class="min-w-0 flex-1">
          <h3 class="text-[11px] font-bold text-slate-600 truncate pr-8">${esc(displayName)}</h3>
          <p class="mono-technical text-[10px] text-slate-400 mt-0.5 font-medium">${esc(shortHwid)}</p>
        </div>
      </div>
      <div class="mt-2.5 flex items-center justify-between text-[10px] font-bold">
        <div class="flex gap-3">
          <span class="flex items-center gap-1 text-slate-400">
            <span class="material-symbols-outlined icon-sm">battery_3_bar</span>${battery}%
          </span>
          <span class="flex items-center gap-1 text-slate-400">
            <span class="material-symbols-outlined icon-sm">device_thermostat</span>${temp}°C
          </span>
        </div>
        <span class="text-slate-400 uppercase text-[10px]">${timeAgo(d.updated_at)}</span>
      </div>
    </div>`;
    };

    const emptyBody =
        '<div class="text-center text-[9px] text-slate-300 py-3 font-semibold uppercase tracking-wider">暂无设备</div>';
    let html = '';

    // Running section
    html += `<div class="dev-section${runningDevs.length ? '' : ' collapsed'}">
      <div class="dev-section-header bg-[#eff6ff] px-3 py-1.5 flex items-center gap-2 sticky top-0 z-10 border-b border-blue-100" onclick="this.parentElement.classList.toggle('collapsed')">
        <span class="material-symbols-outlined section-arrow text-sm text-blue-400">expand_more</span>
        <span class="text-[11px] font-black text-slate-500 uppercase tracking-normal">运行中</span>
      </div>
      <div class="dev-section-body p-2 space-y-2">${runningDevs.length ? runningDevs.map(renderRunningCard).join('') : emptyBody}</div>
    </div>`;

    // Ready section
    html += `<div class="dev-section${readyDevs.length ? '' : ' collapsed'}">
      <div class="dev-section-header bg-[#f8fafc] px-3 py-1.5 flex items-center gap-2 sticky top-0 z-10 border-b border-slate-100 mt-1" onclick="this.parentElement.classList.toggle('collapsed')">
        <span class="material-symbols-outlined section-arrow text-sm text-slate-400">expand_more</span>
        <span class="text-[11px] font-black text-slate-500 uppercase tracking-normal">就绪</span>
      </div>
      <div class="dev-section-body p-2 space-y-2">${readyDevs.length ? readyDevs.map(renderReadyCard).join('') : emptyBody}</div>
    </div>`;

    // Offline section
    html += `<div class="dev-section${offlineDevs.length ? '' : ' collapsed'}">
      <div class="dev-section-header bg-[#f8fafc] px-3 py-1.5 flex items-center gap-2 sticky top-0 z-10 border-b border-slate-100 mt-1" onclick="this.parentElement.classList.toggle('collapsed')">
        <span class="material-symbols-outlined section-arrow text-sm text-slate-400">expand_more</span>
        <span class="text-[11px] font-black text-slate-500 uppercase tracking-normal">离线</span>
      </div>
      <div class="dev-section-body p-2 space-y-2">${offlineDevs.length ? offlineDevs.map(renderOfflineCard).join('') : emptyBody}</div>
    </div>`;

    // Save collapsed state before re-render
    // 记录每个 section 的折叠状态 + 是否有设备内容
    const sectionState = new Map<string, { collapsed: boolean; hadDevices: boolean }>();
    tree.querySelectorAll('.dev-section').forEach(el => {
        const label = el.querySelector('.dev-section-header span:last-child')?.textContent?.trim();
        if (!label) return;
        const body = el.querySelector('.dev-section-body');
        const hadDevices = body ? body.querySelectorAll('.dev-card').length > 0 : false;
        sectionState.set(label, { collapsed: el.classList.contains('collapsed'), hadDevices });
    });

    tree.innerHTML = html;

    // Restore user's manually toggled state
    tree.querySelectorAll('.dev-section').forEach(el => {
        const label = el.querySelector('.dev-section-header span:last-child')?.textContent?.trim();
        if (!label) return;
        const prev = sectionState.get(label);
        if (!prev) return; // 首次渲染，使用默认状态
        if (prev.collapsed && prev.hadDevices) {
            // 用户手动折叠了有设备的分组 → 保持折叠
            el.classList.add('collapsed');
        } else if (!prev.collapsed) {
            // 之前是展开的（用户手动展开或有设备自动展开）→ 保持展开
            el.classList.remove('collapsed');
        }
    });

    // Bind events
    tree.querySelectorAll('.dev-card').forEach(el => {
        const s = (el as HTMLElement).dataset.s!;
        const state = (el as HTMLElement).dataset.state;

        if (state === 'running' || state === 'offline') {
            // 运行中/离线：单击选中，双击查看设备信息
            el.addEventListener('click', e => {
                e.stopPropagation();
                selectDevice(s);
            });
        }

        // 所有状态：双击查看设备信息
        el.addEventListener('dblclick', e => {
            e.stopPropagation();
            showDeviceInfo(s);
        });
    });
}

// Search filter: show/hide device cards based on query
function filterDeviceCards(query: string) {
    const q = query.toLowerCase().trim();
    document.querySelectorAll('.dev-card').forEach(el => {
        const name = el.querySelector('h3')?.textContent?.toLowerCase() || '';
        const sub = el.querySelector('.mono-technical')?.textContent?.toLowerCase() || '';
        const match = !q || name.includes(q) || sub.includes(q);
        (el as HTMLElement).style.display = match ? '' : 'none';
    });
}

function selectDevice(serial: string) {
    // Toggle: clicking the already-selected card deselects it
    if (selectedDevice === serial) {
        selectedDevice = null;
        // Update card selection visual without full re-render
        updateCardSelection();
        const removeBtn = $('#btn-remove-selected') as HTMLButtonElement;
        if (removeBtn) removeBtn.disabled = true;
        // Reset middle pane
        const colMid = $('#col-mid')!;
        colMid.innerHTML =
            '<div class="empty-hint" style="padding:40px;text-align:center">选择任务以查看详情</div>';
        return;
    }

    selectedDevice = serial;

    // Update card selection visual without full re-render
    updateCardSelection();

    // Enable Remove button only for offline devices
    const removeBtn = $('#btn-remove-selected') as HTMLButtonElement;
    const card = document.querySelector(`.dev-card[data-s="${serial}"]`) as HTMLElement | null;
    const isOffline = card?.dataset.state === 'offline';
    if (removeBtn) {
        removeBtn.disabled = !isOffline;
    }

    // Offline 设备不切换中屏任务
    if (!isOffline) {
        loadTasksForDevice(serial);
    }
}

/** Update selection ring on cards without re-rendering the whole tree */
function updateCardSelection() {
    document.querySelectorAll('.dev-card').forEach(el => {
        const s = (el as HTMLElement).dataset.s;
        const state = (el as HTMLElement).dataset.state;
        // 先移除所有可能的 ring 样式
        el.classList.remove('ring-2', 'ring-1', 'ring-blue-300', 'ring-blue-200', 'ring-slate-400');
        if (s === selectedDevice) {
            if (state === 'offline') {
                el.classList.add('ring-2', 'ring-slate-400');
            } else if (state === 'ready') {
                el.classList.add('ring-2', 'ring-blue-200');
            } else {
                el.classList.add('ring-2', 'ring-blue-300');
            }
        }
    });
}

/* ───── Add Device Dialog ───── */
function showAddDeviceDialog() {
    const modal = $('#add-device-modal')!;
    const addrInput = $('#add-device-addr') as HTMLInputElement;
    const nameInput = $('#add-device-name') as HTMLInputElement;
    const errEl = $('#add-device-error')!;

    addrInput.value = '';
    nameInput.value = '';
    errEl.style.display = 'none';
    modal.style.display = 'flex';
    setTimeout(() => addrInput.focus(), 100);
}

function hideAddDeviceDialog() {
    const modal = $('#add-device-modal')!;
    modal.style.display = 'none';
}

async function submitAddDevice() {
    const addrInput = $('#add-device-addr') as HTMLInputElement;
    const nameInput = $('#add-device-name') as HTMLInputElement;
    const errEl = $('#add-device-error')!;
    const submitBtn = $('#add-device-submit') as HTMLButtonElement;

    const address = addrInput.value.trim();
    const name = nameInput.value.trim();

    if (!address) {
        errEl.textContent = '请输入设备 IP 地址';
        errEl.style.display = 'block';
        return;
    }

    // Phase 1: 按钮进入 loading 态
    submitBtn.disabled = true;
    submitBtn.innerHTML = '<span class="btn-spinner"></span> 连接中…';
    submitBtn.classList.add('connecting');
    errEl.style.display = 'none';

    // 等一帧，让浏览器先渲染 spinner 动画
    await nextFrame();

    try {
        await invoke('add_device', { address, name });

        // Phase 2: 关闭对话框，设备列表显示"正在连接"的卡片
        hideAddDeviceDialog();
        const tree = $('#device-tree')!;
        const displayName = name || address;
        tree.innerHTML = `<div class="dev-card device-card-ready border border-blue-100/50 rounded-lg p-2.5 animate-pulse">
      <div class="flex items-start gap-3">
        <div class="h-9 w-9 rounded-lg bg-blue-50 border border-blue-100 flex items-center justify-center text-blue-400 shrink-0">
          <span class="material-symbols-outlined text-xl">smartphone</span>
        </div>
        <div class="min-w-0 flex-1">
          <h3 class="text-[10px] font-bold text-slate-700 truncate">${esc(displayName)}</h3>
          <p class="mono-technical text-[9px] text-blue-500 mt-0.5 font-bold">连接中…</p>
        </div>
      </div>
    </div>`;

        // 等一帧，让浏览器先渲染连接卡片
        await nextFrame();

        // Phase 3: 刷新设备列表（此步骤含 TCP 超时检测，耗时）
        await refreshDevices();
    } catch (e) {
        errEl.textContent = `${e}`;
        errEl.style.display = 'block';
    } finally {
        submitBtn.disabled = false;
        submitBtn.innerHTML = '连接';
        submitBtn.classList.remove('connecting');
    }
}

async function removeSelectedDevice() {
    if (!selectedDevice) return;
    // Only allow removing offline devices
    const card = document.querySelector(
        `.dev-card[data-s="${selectedDevice}"]`,
    ) as HTMLElement | null;
    if (!card || card.dataset.state !== 'offline') return;
    try {
        await invoke('remove_device', { serial: selectedDevice });
        selectedDevice = null;
        const removeBtn = $('#btn-remove-selected') as HTMLButtonElement;
        if (removeBtn) removeBtn.disabled = true;
        await refreshDevices();
    } catch (e) {
        console.error(`移除失败: ${e}`);
    }
}

/* ───── Task View (middle column) ───── */
function loadTasksForDevice(serial: string) {
    const deviceTasks = getTasksForDevice(serial);
    // 合并到全局队列（去重）
    const seen = new Set(globalQueue.map(t => t.id));
    for (const t of deviceTasks) {
        if (!seen.has(t.id)) {
            globalQueue.push(t);
            seen.add(t.id);
        }
    }
    // 优先选中当前设备正在执行的任务，否则选第一个 EXECUTING，再否则选第一个
    activeTask =
        globalQueue.find(t => t.assignedDevice === serial && t.status === 'EXECUTING') ??
        globalQueue.find(t => t.status === 'EXECUTING') ??
        globalQueue[0] ??
        null;
    activeCityIdx = 0;
    renderTaskView();
    loadChainForDevice(serial);
}

function renderTaskView() {
    const mid = document.getElementById('col-mid')!;
    const task = activeTask;
    if (!task) {
        mid.innerHTML = '<div class="empty-hint">从队列中选择任务查看详情</div>';
        return;
    }
    const city = task.cities[activeCityIdx];
    const execDevice = task.assignedDevice ? resolveDeviceLabel(task.assignedDevice) : '';

    // 状态徽章映射
    const statusBadgeMap: Record<
        string,
        { bg: string; text: string; border: string; label: string }
    > = {
        WAITING: {
            bg: 'bg-slate-50',
            text: 'text-slate-500',
            border: 'border-slate-200',
            label: '等待中',
        },
        EXECUTING: {
            bg: 'bg-green-50',
            text: 'text-green-600',
            border: 'border-green-100',
            label: '运行中',
        },
        PAUSED: {
            bg: 'bg-amber-50',
            text: 'text-amber-600',
            border: 'border-amber-100',
            label: '已暂停',
        },
        SUCCESS: {
            bg: 'bg-green-50',
            text: 'text-green-600',
            border: 'border-green-100',
            label: '已完成',
        },
        ERROR: { bg: 'bg-red-50', text: 'text-red-600', border: 'border-red-100', label: '出错' },
    };
    const badge = statusBadgeMap[task.status] ?? statusBadgeMap['WAITING'];

    // 设备信息行
    const deviceLine = task.assignedDevice
        ? `<span class="text-[10px] font-bold text-blue-600 uppercase tracking-tight">执行设备: ${esc(execDevice)}</span>`
        : `<span class="text-[10px] font-bold text-slate-400 uppercase tracking-tight">未分配设备</span>`;

    // 按钮：根据状态显示
    const btnStart =
        task.status === 'WAITING'
            ? `<button onclick="window.__taskStart('${task.id}')" class="px-2.5 py-1.5 rounded-md border border-green-200 bg-green-50 text-[11px] font-black text-green-600 hover:bg-green-100 transition-all flex items-center gap-1.5 uppercase"><span class="material-symbols-outlined text-sm fill-1">play_arrow</span> 启动</button>`
            : '';
    const btnPause =
        task.status === 'EXECUTING'
            ? `<button onclick="window.__taskPause('${task.id}')" class="px-2.5 py-1.5 rounded-md border border-amber-200 bg-amber-50 text-[11px] font-black text-amber-600 hover:bg-amber-100 transition-all flex items-center gap-1.5 uppercase"><span class="material-symbols-outlined text-sm">pause</span> 暂停</button>`
            : '';
    const btnResume =
        task.status === 'PAUSED' || task.status === 'ERROR'
            ? `<button onclick="window.__taskResume('${task.id}')" class="px-2.5 py-1.5 rounded-md border border-green-200 bg-green-50 text-[11px] font-black text-green-600 hover:bg-green-100 transition-all flex items-center gap-1.5 uppercase"><span class="material-symbols-outlined text-sm">resume</span> 继续</button>`
            : '';
    const btnRetry =
        task.status === 'ERROR' || task.status === 'SUCCESS'
            ? `<button onclick="window.__taskRetry('${task.id}')" class="px-2.5 py-1.5 rounded-md border border-blue-200 bg-blue-50 text-[11px] font-black text-blue-600 hover:bg-blue-100 transition-all flex items-center gap-1.5 uppercase"><span class="material-symbols-outlined text-sm">replay</span> 重跑</button>`
            : '';
    const btnStop =
        task.status === 'EXECUTING' || task.status === 'PAUSED'
            ? `<button onclick="window.__taskStop('${task.id}')" class="px-2.5 py-1.5 rounded-md border border-red-200 bg-red-50 text-[11px] font-black text-red-600 hover:bg-red-100 transition-all flex items-center gap-1.5 uppercase"><span class="material-symbols-outlined text-sm">stop</span> 停止</button>`
            : '';

    mid.innerHTML = `
    <!-- Task Header -->
    <div class="bg-white rounded-md border border-[var(--panel-border)] shadow-sm p-3 mb-4 flex items-center justify-between shrink-0">
      <div class="flex items-center space-x-4">
        <div class="h-11 w-11 rounded-xl task-icon-glow text-white flex items-center justify-center">
          <span class="material-symbols-outlined text-2xl fill-1">automation</span>
        </div>
        <div>
          <div class="flex items-center space-x-2">
            <h2 class="text-sm font-black text-slate-800">${task.name}</h2>
            <span class="px-1.5 py-0.5 ${badge.bg} ${badge.text} text-[10px] font-black rounded border ${badge.border} uppercase tracking-tighter">${badge.label}</span>
          </div>
          <div class="flex flex-col mt-0.5">
            ${deviceLine}
            <div class="flex items-center text-[10px] text-slate-400 font-medium mt-0.5">
              <span class="material-symbols-outlined icon-xs text-[10px] mr-1">location_on</span>${task.cities.length} 个城市
              <span class="mx-2 text-slate-200">|</span>
              <span class="uppercase tracking-tighter">${task.cities.reduce((s, c) => s + c.total, 0)} 个关键词</span>
            </div>
          </div>
        </div>
      </div>
      <div class="flex items-center space-x-1.5">
        ${btnStart}${btnPause}${btnResume}${btnRetry}${btnStop}
      </div>
    </div>

    <!-- City Cards -->
    <div class="flex gap-2.5 mb-4 shrink-0 overflow-x-auto pb-1 scrollbar-hide">
      ${task.cities
          .map((c, i) => {
              const isActive = i === activeCityIdx;
              const borderCls = isActive
                  ? 'border-2 border-blue-500 shadow-md'
                  : 'border border-slate-200';
              const statusIcon =
                  c.status === 'done'
                      ? '<span class="material-symbols-outlined text-green-500 icon-sm fill-1">check_circle</span>'
                      : c.status === 'active'
                        ? `<div class="h-1.5 w-1.5 rounded-full bg-blue-500"></div>`
                        : '<span class="material-symbols-outlined text-slate-300 icon-sm">schedule</span>';
              const nameWeight = isActive
                  ? 'font-bold text-slate-900'
                  : 'font-semibold text-slate-500';
              const barBg =
                  c.status === 'done'
                      ? 'bg-green-50'
                      : c.status === 'active'
                        ? 'bg-slate-100'
                        : 'bg-slate-50';
              const barFill =
                  c.status === 'done'
                      ? 'bg-green-500'
                      : c.status === 'active'
                        ? 'bg-blue-500'
                        : 'bg-slate-200';
              const pct = c.status === 'done' ? 100 : c.progress;
              const statsLabel =
                  c.status === 'done'
                      ? '<span class="text-[10px] text-slate-400 font-bold uppercase">已完成</span>'
                      : c.status === 'active'
                        ? `<span class="text-[10px] text-slate-400 font-bold uppercase">${c.done}/${c.total} 关键词</span>`
                        : '<span class="text-[10px] text-slate-400 font-bold uppercase">等待中</span>';
              const pctLabel =
                  c.status === 'done'
                      ? '<span class="text-[10px] font-black text-green-600">100%</span>'
                      : c.status === 'active'
                        ? `<span class="text-[10px] font-black text-blue-600">${c.progress}%</span>`
                        : '';
              const cardBg =
                  c.status === 'done'
                      ? 'bg-green-50/50'
                      : c.status === 'active'
                        ? 'bg-blue-50/40'
                        : 'bg-slate-50/50';
              return `
        <div class="${cardBg} rounded-md ${borderCls} p-2.5 cursor-pointer ${!isActive ? 'hover:bg-slate-50' : ''} transition-all relative overflow-hidden" style="width:180px;min-width:180px;flex-shrink:0" onclick="window.__switchCity(${i})">
          <div class="flex items-center justify-between mb-1.5">
            <div class="flex items-center space-x-2">
              <span class="material-symbols-outlined icon-sm text-blue-400">location_city</span>
              <span class="text-[11px] ${nameWeight}">${c.name}</span>
            </div>
            ${statusIcon}
          </div>
          <div class="text-[9px] text-slate-400 truncate mb-1.5" title="${c.poi}">
            <span class="material-symbols-outlined icon-xs text-slate-300 align-middle mr-0.5">location_on</span>${c.poi}
          </div>
          <div class="w-full h-1.5 ${barBg} rounded-full overflow-hidden">
            <div class="h-full ${barFill} rounded-full" style="width: ${pct}%"></div>
          </div>
          <div class="flex justify-between mt-1.5">
            ${statsLabel}
            ${pctLabel}
          </div>
        </div>`;
          })
          .join('')}
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
          <input class="w-full pl-9 pr-3 py-1.5 bg-white border border-slate-200 rounded-lg text-xs focus:ring-2 focus:ring-blue-100 focus:border-blue-500 outline-none transition-all" placeholder="搜索关键词..." type="text" id="kw-filter-input" oninput="window.__filterKw(this.value)" />
        </div>
      </div>
      <div class="p-4 overflow-y-auto flex-1">
        <div class="grid gap-2" style="grid-template-columns: repeat(5, minmax(0, 1fr))" id="kw-grid">
          ${city.keywords
              .map(k => {
                  if (k.status === 'ok') {
                      return `<div class="kw-item flex items-center justify-between p-2 rounded bg-green-100/50 border border-green-200 transition-all hover:bg-green-100">
        <div class="flex items-center min-w-0">
          <span class="material-symbols-outlined icon-sm text-green-500 mr-1.5 fill-1">check_circle</span>
          <span class="text-[12px] font-semibold text-slate-700 truncate">${k.name}</span>
        </div>
      </div>`;
                  } else if (k.status === 'run') {
                      return `<div class="kw-item flex items-center p-2 rounded bg-blue-100/50 border border-blue-400 keyword-active ring-2 ring-blue-100">
        <div class="h-2 w-2 rounded-full bg-blue-500 mr-1.5 animate-pulse"></div>
        <span class="text-[12px] font-bold text-blue-700 truncate">${k.name}</span>
      </div>`;
                  } else {
                      return `<div class="kw-item flex items-center p-2 rounded bg-slate-100/50 border border-slate-200 hover:border-slate-300 transition-all cursor-pointer">
        <span class="material-symbols-outlined icon-sm text-slate-300 mr-1.5">circle</span>
        <span class="text-[12px] font-medium text-slate-500 truncate">${k.name}</span>
      </div>`;
                  }
              })
              .join('')}
        </div>
      </div>
    </div>
  `;
}

/* ───── Global onclick handlers (Tauri WebView compatible) ───── */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
(window as any).__switchTask = (taskId: string) => {
    const task = globalQueue.find(t => t.id === taskId);
    if (!task || task === activeTask) return;
    activeTask = task;
    activeCityIdx = 0;
    // 切换任务时清除设备选中状态
    selectedDevice = null;
    updateCardSelection();
    renderTaskView();
    loadChainForDevice('');
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(window as any).__switchCity = (idx: number) => {
    if (idx === activeCityIdx) return;
    activeCityIdx = idx;
    renderTaskView();
    // 自动滚动到选中的城市卡片
    const cityContainer = document.querySelector('#col-mid .overflow-x-auto');
    const cards = cityContainer?.querySelectorAll('[onclick*="__switchCity"]');
    if (cards && cards[idx]) {
        cards[idx].scrollIntoView({ behavior: 'smooth', block: 'nearest', inline: 'center' });
    }
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(window as any).__filterKw = (val: string) => {
    const q = val.toLowerCase();
    document.querySelectorAll('#kw-grid .kw-item').forEach(el => {
        const name = el.textContent?.toLowerCase() || '';
        (el as HTMLElement).style.display = name.includes(q) ? '' : 'none';
    });
};

/** 获取就绪设备列表（在线、未被任务占用） */
function getReadySerials(): string[] {
    const assigned = getAssignedDeviceSerials();
    const readyCards = document.querySelectorAll('.dev-card[data-state="ready"]');
    const serials: string[] = [];
    readyCards.forEach(el => {
        const s = (el as HTMLElement).dataset.s;
        if (s && !assigned.has(s)) serials.push(s);
    });
    return serials;
}

/** 从 DB 查询设备列表构建 propsMap（供 mock-data 任务分配使用） */
async function buildPropsMap(): Promise<Map<string, { battery_level: number }>> {
    const devs: DeviceRow[] = await invoke('list_devices');
    const m = new Map<string, { battery_level: number }>();
    for (const d of devs) {
        m.set(d.serial, { battery_level: d.battery_level });
    }
    return m;
}

/** 任务操作后统一刷新（查 DB + 渲染，DB 查询 <1ms） */
async function afterTaskAction() {
    await refreshDevices();
    renderTaskView();
    loadChainForDevice(selectedDevice ?? '');
}

// 注册执行引擎的 tick 回调（每 1.5 秒关键词推进时触发 UI 刷新）
setTaskTickCallback(() => {
    refreshDevices(); // 刷新设备卡片中的进度条
    renderTaskView();
    loadChainForDevice(selectedDevice ?? '');
});

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(window as any).__taskStart = async (taskId: string) => {
    const readySerials = getReadySerials();
    if (!readySerials.length) {
        showToast('当前没有就绪的设备，请先连接设备');
        return;
    }
    const propsMap = await buildPropsMap();
    const result = startTask(taskId, readySerials, propsMap);
    if (!result.ok) {
        showToast(result.error ?? '启动失败');
        return;
    }
    if (result.serial) selectedDevice = result.serial;
    startTaskExecution(taskId);
    await afterTaskAction();
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(window as any).__taskPause = async (taskId: string) => {
    stopTaskExecution(taskId);
    const result = pauseTask(taskId);
    if (!result.ok) {
        showToast(result.error ?? '暂停失败');
        return;
    }
    await afterTaskAction();
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(window as any).__taskResume = async (taskId: string) => {
    const readySerials = getReadySerials();
    const propsMap = await buildPropsMap();
    const result = resumeTask(taskId, readySerials, propsMap);
    if (!result.ok) {
        showToast(result.error ?? '继续失败');
        return;
    }
    if (result.serial) selectedDevice = result.serial;
    startTaskExecution(taskId);
    await afterTaskAction();
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(window as any).__taskStop = async (taskId: string) => {
    stopTaskExecution(taskId);
    const result = stopTask(taskId);
    if (!result.ok) {
        showToast(result.error ?? '停止失败');
        return;
    }
    selectedDevice = null;
    await afterTaskAction();
};

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(window as any).__taskRetry = async (taskId: string) => {
    const readySerials = getReadySerials();
    const propsMap = await buildPropsMap();
    const result = retryTask(taskId, readySerials, propsMap);
    if (!result.ok) {
        showToast(result.error ?? '重跑失败');
        return;
    }
    if (result.serial) selectedDevice = result.serial;
    startTaskExecution(taskId);
    await afterTaskAction();
};

/* ───── Global Queue (right column, code.html style) ───── */
/** 从 DOM 设备卡片中解析 hw_serial（避免显示 IP 地址） */
function resolveDeviceLabel(serial: string): string {
    const card = document.querySelector(`.dev-card[data-s="${serial}"]`);
    if (card) {
        const hwid = card.querySelector('.mono-technical')?.textContent?.trim();
        if (hwid) return hwid;
    }
    return serial.substring(0, 14);
}

function loadChainForDevice(_serial: string) {
    const cards = $('#chain-cards')!;

    const queueSub = document.querySelector('.queue-sub');
    const executingCount = globalQueue.filter(t => t.status === 'EXECUTING').length;
    if (queueSub)
        queueSub.textContent = `${executingCount} 个执行中 · 共 ${globalQueue.length} 个任务`;

    cards.innerHTML = globalQueue
        .map(q => {
            const isActive = q === activeTask;
            const deviceSub = q.assignedDevice ? resolveDeviceLabel(q.assignedDevice) : '';
            const kwTotal = q.cities.reduce((s, c) => s + c.total, 0);
            const cityCount = q.cities.length;
            const ring = isActive ? 'ring-2 ring-blue-200' : '';

            // 城市 + 关键词统计行
            const statsRow = `
        <div class="flex gap-4 text-slate-500">
          <div class="flex items-center gap-1">
            <span class="material-symbols-outlined icon-xs text-[14px]">domain</span>
            <span class="text-[10px] font-medium">${cityCount} 城市</span>
          </div>
          <div class="flex items-center gap-1">
            <span class="material-symbols-outlined icon-xs text-[14px]">sell</span>
            <span class="text-[10px] font-medium">${kwTotal} 关键词</span>
          </div>
        </div>`;

            if (q.status === 'EXECUTING') {
                const kwDone = q.cities.reduce((s, c) => s + c.done, 0);
                const pct = kwTotal > 0 ? Math.round((kwDone / kwTotal) * 100) : 0;
                return `
      <div class="bg-blue-50/40 border border-blue-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all hover:shadow-md ${ring}" onclick="window.__switchTask('${q.id}')">
        <div class="w-1 self-stretch bg-[#2563EB]"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-xs text-slate-900 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="flex items-center gap-1.5 bg-slate-100 px-2 py-0.5 rounded-full shrink-0">
              <span class="w-1.5 h-1.5 bg-[#2563EB] rounded-full animate-pulse"></span>
              <span class="text-[10px] font-bold text-[#2563EB]">执行中</span>
            </div>
          </div>
          ${statsRow}
          <div class="flex justify-between items-center mt-2">
            <span class="text-[10px] font-semibold text-slate-500">当前进度</span>
            <span class="text-[10px] font-bold text-[#2563EB]">${pct}%</span>
          </div>
          <div class="absolute bottom-0 left-1 right-0 h-[2px] bg-slate-100 overflow-hidden">
            <div class="h-full bg-[#2563EB]" style="width: ${pct}%"></div>
          </div>
        </div>
      </div>`;
            } else if (q.status === 'PAUSED') {
                const kwDone = q.cities.reduce((s, c) => s + c.done, 0);
                const pct = kwTotal > 0 ? Math.round((kwDone / kwTotal) * 100) : 0;
                return `
      <div class="bg-amber-50/40 border border-amber-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all hover:shadow-md ${ring}" onclick="window.__switchTask('${q.id}')">
        <div class="w-1 self-stretch bg-amber-400"></div>
        <div class="flex-1 p-3 flex flex-col">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-xs text-slate-900 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="bg-slate-100 px-2 py-0.5 rounded-full shrink-0">
              <span class="text-[10px] font-bold text-amber-600">已暂停</span>
            </div>
          </div>
          <p class="mono-technical text-[10px] text-amber-600 font-medium mb-2">设备: ${esc(deviceSub)}</p>
          ${statsRow}
          <div class="mt-auto flex justify-between items-center mb-1 mt-3">
            <span class="text-[10px] font-medium text-slate-400">暂停于</span>
            <span class="text-[10px] font-bold text-amber-600">${pct}%</span>
          </div>
          <div class="absolute bottom-0 left-1 right-0 h-[2px] bg-slate-100 overflow-hidden">
            <div class="h-full bg-amber-400" style="width: ${pct}%"></div>
          </div>
        </div>
      </div>`;
            } else if (q.status === 'WAITING' || q.status === 'SCHEDULED') {
                return `
      <div class="bg-slate-50/60 border border-slate-200 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all hover:shadow-md ${ring}" onclick="window.__switchTask('${q.id}')">
        <div class="w-1 self-stretch bg-[#64748B]"></div>
        <div class="flex-1 p-3">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-xs text-slate-900 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="bg-slate-100 px-2 py-0.5 rounded-full shrink-0">
              <span class="text-[11px] font-bold text-[#64748B]">等待中</span>
            </div>
          </div>
          ${statsRow}
        </div>
      </div>`;
            } else if (q.status === 'SUCCESS') {
                return `
      <div class="bg-green-50/40 border border-green-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer opacity-80 transition-all hover:opacity-100 ${ring}" onclick="window.__switchTask('${q.id}')">
        <div class="w-1 self-stretch bg-[#10B981]"></div>
        <div class="flex-1 p-3">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-xs text-slate-900 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="bg-slate-100 px-2 py-0.5 rounded-full shrink-0">
              <span class="text-[10px] font-bold text-[#10B981]">已完成</span>
            </div>
          </div>
          ${statsRow}
        </div>
      </div>`;
            } else if (q.status === 'ERROR') {
                return `
      <div class="bg-red-50/40 border border-red-100 rounded-lg shadow-sm relative overflow-hidden flex cursor-pointer transition-all hover:shadow-md ${ring}" onclick="window.__switchTask('${q.id}')">
        <div class="w-1 self-stretch bg-red-500"></div>
        <div class="flex-1 p-3">
          <div class="flex justify-between items-start mb-2">
            <h4 class="font-semibold text-sm text-slate-900 leading-tight truncate pr-2">${esc(q.name)}</h4>
            <div class="bg-slate-100 px-2 py-0.5 rounded-full shrink-0">
              <span class="text-[10px] font-bold text-red-500">出错</span>
            </div>
          </div>
          <p class="mono-technical text-[10px] text-slate-400 font-medium mb-2">设备: ${esc(deviceSub)}</p>
          <p class="text-[10px] text-red-500 font-medium">执行异常 · 等待操作</p>
        </div>
      </div>`;
            } else {
                return '';
            }
        })
        .join('');
}

/* ───── Device Info Modal ───── */
async function showDeviceInfo(serial: string) {
    const m = $('#modal')!;
    const b = $('#modal-body')!;
    m.style.display = 'flex';
    b.innerHTML = '<div class="text-center p-4.5"><span class="spinner"></span></div>';
    try {
        const i: DeviceProperties = await invoke('get_device_info', { serial });
        const typeLabel = i.device_type === 'usb' ? 'USB' : 'WiFi';
        const typeIcon =
            i.device_type === 'usb'
                ? '<span class="material-symbols-outlined text-base">usb</span>'
                : '<span class="material-symbols-outlined text-base">wifi</span>';
        const batteryPct = i.battery_level ?? 0;
        const batteryColor = batteryPct < 30 ? 'text-orange' : 'text-green';
        const tempVal = i.battery_temperature ?? 0;
        const tempColor = tempVal > 40 ? 'text-orange' : 'text-s500';

        b.innerHTML = `
      <!-- Device Header -->
      <div class="flex items-center gap-3 mb-5">
        <div class="w-12 h-12 rounded-lg bg-gradient-to-br from-blue to-blue-dark flex items-center justify-center shrink-0 shadow-[0_4px_12px_rgba(37,99,235,.3)]">
          <span class="material-symbols-outlined text-2xl text-white">smartphone</span>
        </div>
        <div>
          <div class="text-[15px] font-bold text-s900">${esc(i.brand)} ${esc(i.model)}</div>
          <div class="flex items-center gap-1.5 mt-0.5">
            <span class="inline-flex items-center gap-1 text-[11px] font-semibold text-s400">${typeIcon} ${typeLabel}</span>
            <span class="text-s200">·</span>
            <span class="text-[11px] font-mono text-s400">${esc(i.serial)}</span>
          </div>
        </div>
      </div>

      <!-- Stats Row -->
      <div class="flex gap-2 mb-5">
        <div class="flex-1 px-3 py-2.5 rounded-lg bg-s50 border border-s100 text-center">
          <div class="text-[10px] font-bold text-s400 uppercase tracking-wider mb-1">电量</div>
          <div class="text-[16px] font-bold ${batteryColor}">${batteryPct}%</div>
        </div>
        <div class="flex-1 px-3 py-2.5 rounded-lg bg-s50 border border-s100 text-center">
          <div class="text-[10px] font-bold text-s400 uppercase tracking-wider mb-1">温度</div>
          <div class="text-[16px] font-bold ${tempColor}">${tempVal}°C</div>
        </div>
        <div class="flex-1 px-3 py-2.5 rounded-lg bg-s50 border border-s100 text-center">
          <div class="text-[10px] font-bold text-s400 uppercase tracking-wider mb-1">Android</div>
          <div class="text-[14px] font-bold text-s700">${esc(i.android_version)}</div>
        </div>
      </div>

      <!-- Detail Rows -->
      <div class="flex flex-col gap-0 rounded-lg border border-s100 overflow-hidden">
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-white border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">序列号</span>
          <span class="text-[12px] font-mono font-medium text-s700">${esc(i.serial)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-s50 border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">品牌</span>
          <span class="text-[12px] font-medium text-s700">${esc(i.brand)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-white border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">型号</span>
          <span class="text-[12px] font-medium text-s700">${esc(i.model)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-s50 border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">SDK 版本</span>
          <span class="text-[12px] font-mono font-medium text-s700">${esc(i.sdk_version)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-white">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">分辨率</span>
          <span class="text-[12px] font-mono font-medium text-s700">${esc(i.display_resolution)}</span>
        </div>
      </div>`;
    } catch (e) {
        b.innerHTML = `<p class="text-red text-center py-4">获取失败: ${e}</p>`;
    }
}

/* ───── Utility ───── */
function esc(t: string) {
    return t
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
}

/** 等待浏览器完成一帧渲染（双 rAF 保证 paint 完成） */
function nextFrame(): Promise<void> {
    return new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r())));
}

/* ───── Init ───── */
window.addEventListener('DOMContentLoaded', () => {
    splash();

    // 初始化全局任务队列
    globalQueue = getTasksForDevice('*');
    // 默认选中第一个任务
    if (globalQueue.length > 0 && !activeTask) {
        activeTask = globalQueue[0];
        activeCityIdx = 0;
        renderTaskView();
    }
    loadChainForDevice('');

    // Search toggle
    $('#btn-search-dev')?.addEventListener('click', () => {
        const box = $('#dev-search');
        const searchInp = $('#dev-search-input') as HTMLInputElement | null;
        if (!box || !searchInp) return;
        const visible = box.style.display !== 'none';
        box.style.display = visible ? 'none' : 'block';
        if (!visible) {
            searchInp.value = '';
            searchInp.focus();
        } else filterDeviceCards('');
    });

    $('#dev-search-input')?.addEventListener('input', e => {
        filterDeviceCards((e.target as HTMLInputElement).value);
    });

    // Add Device (WiFi)
    $('#btn-add-device')?.addEventListener('click', showAddDeviceDialog);
    $('#add-device-close')?.addEventListener('click', hideAddDeviceDialog);
    $('#add-device-cancel')?.addEventListener('click', hideAddDeviceDialog);
    $('#add-device-submit')?.addEventListener('click', submitAddDevice);
    $('#add-device-modal')?.addEventListener('click', e => {
        if (e.target === e.currentTarget) hideAddDeviceDialog();
    });
    // Enter key to submit
    $('#add-device-addr')?.addEventListener('keydown', e => {
        if ((e as KeyboardEvent).key === 'Enter') submitAddDevice();
    });
    $('#add-device-name')?.addEventListener('keydown', e => {
        if ((e as KeyboardEvent).key === 'Enter') submitAddDevice();
    });

    // Remove Selected
    $('#btn-remove-selected')?.addEventListener('click', removeSelectedDevice);

    // Modal
    $('#modal-close')?.addEventListener('click', () => {
        ($('#modal') as HTMLElement).style.display = 'none';
    });
    $('#modal')?.addEventListener('click', e => {
        if (e.target === e.currentTarget) (e.currentTarget as HTMLElement).style.display = 'none';
    });

    // ── Settings ──
    const settingsOverlay = $('#settings-overlay') as HTMLElement;

    $('#btn-settings')?.addEventListener('click', async () => {
        // Load settings from backend
        try {
            const settings = await invoke<Record<string, string>>('get_settings');
            ($('#set-mqtt-host') as HTMLInputElement).value = settings.mqtt_host || '';
            ($('#set-mqtt-port') as HTMLInputElement).value = settings.mqtt_port || '1883';
            ($('#set-mqtt-client-id') as HTMLInputElement).value = settings.mqtt_client_id || '';
            ($('#set-mqtt-username') as HTMLInputElement).value = settings.mqtt_username || '';
            ($('#set-mqtt-password') as HTMLInputElement).value = settings.mqtt_password || '';
        } catch {
            /* ignore */
        }
        // Check MQTT status
        try {
            const s = await invoke<string>('mqtt_status');
            updateMqttStatusUI(s);
        } catch {
            /* ignore */
        }
        settingsOverlay.style.display = 'flex';
    });

    $('#settings-close')?.addEventListener('click', () => (settingsOverlay.style.display = 'none'));
    $('#settings-cancel')?.addEventListener(
        'click',
        () => (settingsOverlay.style.display = 'none'),
    );
    settingsOverlay?.addEventListener('click', e => {
        if (e.target === e.currentTarget) settingsOverlay.style.display = 'none';
    });

    $('#settings-save')?.addEventListener('click', async () => {
        const settings = {
            mqtt_host: ($('#set-mqtt-host') as HTMLInputElement).value,
            mqtt_port: ($('#set-mqtt-port') as HTMLInputElement).value,
            mqtt_client_id: ($('#set-mqtt-client-id') as HTMLInputElement).value,
            mqtt_username: ($('#set-mqtt-username') as HTMLInputElement).value,
            mqtt_password: ($('#set-mqtt-password') as HTMLInputElement).value,
        };
        try {
            await invoke('save_settings', { settings });
            settingsOverlay.style.display = 'none';
        } catch (e) {
            console.error('Save settings failed:', e);
        }
    });

    $('#btn-mqtt-connect')?.addEventListener('click', async () => {
        try {
            await invoke('mqtt_connect');
            updateMqttStatusUI('connecting');
        } catch (e) {
            updateMqttStatusUI(`error:${e}`);
        }
    });

    $('#btn-mqtt-disconnect')?.addEventListener('click', async () => {
        try {
            await invoke('mqtt_disconnect');
            updateMqttStatusUI('disconnected');
        } catch (e) {
            console.error('MQTT disconnect failed:', e);
        }
    });

    // Resizers removed — using fixed width layout per code.html

    // 监听后台 ADB 设备变更事件，自动刷新
    listen('devices-changed', async () => {
        // 先刷新设备列表
        await refreshDevices();
        // 获取在线设备，释放离线设备上的任务
        const devs: DeviceRow[] = await invoke('list_devices');
        const onlineSerials = new Set(devs.filter(d => d.state !== 'Offline').map(d => d.serial));
        const released = releaseTasksForOfflineDevices(onlineSerials);
        if (released > 0) {
            renderTaskView();
            loadChainForDevice(selectedDevice ?? '');
        }
    });

    // 监听 MQTT 状态事件
    listen<string>('mqtt-status', event => {
        updateMqttStatusUI(event.payload);
    });

    // Auto-refresh after splash
    setTimeout(refreshDevices, 2400);
});

function updateMqttStatusUI(status: string) {
    const dot = $('#mqtt-dot');
    const text = $('#mqtt-status-text');
    const btnConn = $('#btn-mqtt-connect') as HTMLButtonElement;
    const btnDisc = $('#btn-mqtt-disconnect') as HTMLButtonElement;

    if (dot) {
        dot.className = 'mqtt-dot';
        if (status === 'connected') dot.classList.add('connected');
        else if (status.startsWith('error')) dot.classList.add('error');
    }
    if (text) {
        if (status === 'connected') text.textContent = '已连接';
        else if (status === 'connecting') text.textContent = '连接中...';
        else if (status === 'disconnected') text.textContent = '未连接';
        else if (status.startsWith('error:')) text.textContent = `错误: ${status.slice(6)}`;
        else text.textContent = status;
    }
    if (btnConn) btnConn.disabled = status === 'connected' || status === 'connecting';
    if (btnDisc) btnDisc.disabled = status !== 'connected';
}
