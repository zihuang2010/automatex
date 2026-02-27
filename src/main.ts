import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { type MockTask, getTasksForDevice } from './mock-data';

/* ===== Types ===== */
interface DeviceInfo {
    serial: string;
    name: string;
    state: string;
    device_type: string;
}
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

const $ = (s: string) => document.querySelector(s) as HTMLElement | null;

/** 获取设备显示名称，自动回退 unknown → d.name → serial */
function getDeviceName(d: DeviceInfo, p: DeviceProperties | null): string {
    if (p) {
        const brand = p.brand && p.brand !== 'unknown' ? p.brand : '';
        const model = p.model && p.model !== 'unknown' ? p.model : '';
        const combined = `${brand} ${model}`.trim();
        if (combined) return combined;
    }
    return d.name || d.serial;
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
const devicePropsCache: Map<string, DeviceProperties> = new Map();

async function refreshDevices() {
    const tree = $('#device-tree')!;
    tree.innerHTML = '<div class="text-center p-5"><span class="spinner"></span></div>';

    try {
        const devs: DeviceInfo[] = await invoke('list_devices');

        // Update dev count badge
        const devCount = $('#dev-count');
        if (devCount) devCount.textContent = `${devs.length} 台`;

        if (!devs.length) {
            renderDeviceCards([], []);
            return;
        }

        const propsPromises = devs.map(d => {
            if (d.state === 'Offline') {
                return Promise.resolve(null as DeviceProperties | null);
            }

            return invoke<DeviceProperties>('get_device_info', { serial: d.serial }).catch(
                () => null as DeviceProperties | null,
            );
        });
        const propsResults = await Promise.all(propsPromises);

        devicePropsCache.clear();
        propsResults.forEach((p, i) => {
            if (p) devicePropsCache.set(devs[i].serial, p);
        });

        renderDeviceCards(devs, propsResults);

        // 检查当前选中设备是否仍在列表中
        if (selectedDevice && !devs.some(d => d.serial === selectedDevice)) {
            selectedDevice = null;
        }

        if (!selectedDevice) {
            // 只自动选中运行中的设备，就绪/离线设备不自动选中
            const firstRunning = devs.find(d => d.state === 'device');
            if (firstRunning) selectDevice(firstRunning.serial);
        }
    } catch (e) {
        tree.innerHTML = `<div class="empty-hint text-red">错误: ${e}</div>`;
    }
}

function renderDeviceCards(devs: DeviceInfo[], propsResults: (DeviceProperties | null)[]) {
    const tree = $('#device-tree')!;
    const devCount = $('#dev-count');
    if (devCount) devCount.textContent = `${devs.length} 台`;

    // Split devices: Running / Ready / Offline
    const executingSerials = new Set(
        globalQueue
            .filter(t => t.status === 'EXECUTING' && t.assignedDevice)
            .map(t => t.assignedDevice!),
    );

    const runningDevs: { d: DeviceInfo; p: DeviceProperties | null }[] = [];
    const readyDevs: { d: DeviceInfo; p: DeviceProperties | null }[] = [];
    const offlineDevs: { d: DeviceInfo; p: DeviceProperties | null }[] = [];

    devs.forEach((d, idx) => {
        const p = propsResults[idx];
        if (d.state === 'Offline') {
            offlineDevs.push({ d, p });
        } else if (executingSerials.has(d.serial)) {
            runningDevs.push({ d, p });
        } else {
            readyDevs.push({ d, p });
        }
    });

    const renderRunningCard = (item: { d: DeviceInfo; p: DeviceProperties | null }) => {
        const { d, p } = item;
        const displayName = getDeviceName(d, p);
        const battery = p?.battery_level ?? 0;
        const temp = p?.battery_temperature ?? 0;
        const shortHwid = d.serial.length > 16 ? d.serial.substring(0, 16) + '…' : d.serial;
        const devTask = globalQueue.find(t => t.assignedDevice === d.serial);
        const taskLabel = devTask ? devTask.name : '空闲';
        const progress = devTask?.cities.find(c => c.status === 'active')?.progress ?? 0;
        const batteryIcon =
            battery > 80 ? 'battery_charging_80' : battery > 50 ? 'battery_5_bar' : 'battery_3_bar';
        const isSel = d.serial === selectedDevice;

        return `<div class="dev-card device-card-running border rounded-md p-2.5 transition-all hover:shadow-sm cursor-pointer ${isSel ? 'ring-2 ring-blue-300' : ''}" data-s="${esc(d.serial)}" data-state="running">
      <div class="flex items-start gap-3">
        <div class="h-9 w-9 rounded-lg bg-white border border-blue-100 flex items-center justify-center text-blue-500 shrink-0">
          <span class="material-symbols-outlined text-xl">smartphone</span>
        </div>
        <div class="min-w-0 flex-1">
          <div class="flex justify-between items-center mb-0.5">
            <h3 class="text-[10px] font-bold text-slate-900 truncate">${esc(displayName)}</h3>
            <div class="pulsing-dot scale-90"></div>
          </div>
          <p class="mono-technical text-[9px] text-slate-500 font-medium">${esc(shortHwid)}</p>
        </div>
      </div>
      <div class="mt-2.5">
        <div class="flex justify-between items-end mb-1">
          <span class="text-[8px] font-bold text-blue-600 uppercase tracking-tight">${esc(taskLabel)}</span>
          <span class="text-[9px] font-black text-blue-600">${progress}%</span>
        </div>
        <div class="w-full h-1 bg-white/80 rounded-full overflow-hidden">
          <div class="h-full bg-blue-500 rounded-full" style="width: ${progress}%"></div>
        </div>
      </div>
      <div class="mt-2.5 flex items-center gap-3 text-[9px] font-bold">
        <span class="flex items-center gap-1 text-blue-600">
          <span class="material-symbols-outlined icon-sm fill-1">${batteryIcon}</span>${battery}%
        </span>
        <span class="flex items-center gap-1 text-slate-500">
          <span class="material-symbols-outlined icon-sm">device_thermostat</span>${temp}°C
        </span>
      </div>
    </div>`;
    };

    const renderReadyCard = (item: { d: DeviceInfo; p: DeviceProperties | null }) => {
        const { d, p } = item;
        const displayName = getDeviceName(d, p);
        const battery = p?.battery_level ?? 0;
        const temp = p?.battery_temperature ?? 0;
        const hwid = p?.serial && p.serial !== 'unknown' ? p.serial : d.serial;
        const shortHwid = hwid.length > 16 ? hwid.substring(0, 16) + '…' : hwid;
        const batteryIcon =
            battery > 80 ? 'battery_full' : battery > 50 ? 'battery_5_bar' : 'battery_3_bar';
        const batteryColor =
            battery > 80 ? 'text-green-600' : battery > 30 ? 'text-blue-600' : 'text-orange-500';
        const tempColor = temp > 40 ? 'text-orange-500' : 'text-slate-500';
        const isSel = d.serial === selectedDevice;

        return `<div class="dev-card device-card-ready border rounded-md p-2.5 transition-all hover:border-blue-200 cursor-pointer relative ${isSel ? 'ring-2 ring-blue-200' : ''}" data-s="${esc(d.serial)}" data-state="ready">
      <div class="absolute top-2.5 right-2.5 px-1.5 py-0.5 bg-blue-50 text-blue-600 text-[10px] font-black rounded border border-blue-100 uppercase tracking-normal">就绪</div>
      <div class="flex items-start gap-3">
        <div class="h-9 w-9 rounded-lg bg-slate-50 border border-slate-100 flex items-center justify-center text-slate-400 shrink-0">
          <span class="material-symbols-outlined text-xl">smartphone</span>
        </div>
        <div class="min-w-0 flex-1">
          <h3 class="text-[10px] font-bold text-slate-700 truncate pr-8">${esc(displayName)}</h3>
          <p class="mono-technical text-[9px] text-slate-500 mt-0.5 font-medium">${esc(shortHwid)}</p>
        </div>
      </div>
      <div class="mt-2.5 flex items-center gap-3 text-[9px] font-bold">
        <span class="flex items-center gap-1 ${batteryColor}">
          <span class="material-symbols-outlined icon-sm fill-1">${batteryIcon}</span>${battery}%
        </span>
        <span class="flex items-center gap-1 ${tempColor}">
          <span class="material-symbols-outlined icon-sm">device_thermostat</span>${temp}°C
        </span>
      </div>
    </div>`;
    };

    const renderOfflineCard = (item: { d: DeviceInfo; p: DeviceProperties | null }) => {
        const { d, p } = item;
        const displayName = getDeviceName(d, p);
        const battery = p?.battery_level ?? 0;
        const temp = p?.battery_temperature ?? 0;
        const hwid = p?.serial && p.serial !== 'unknown' ? p.serial : d.serial;
        const shortHwid = hwid.length > 16 ? hwid.substring(0, 16) + '…' : hwid;
        const isSel = d.serial === selectedDevice;

        return `<div class="dev-card device-card-offline border rounded-md p-2.5 cursor-pointer relative ${isSel ? 'ring-2 ring-slate-400' : ''}" data-s="${esc(d.serial)}" data-state="offline">
      <div class="absolute top-2.5 right-2.5 px-1.5 py-0.5 bg-slate-200 text-slate-500 text-[7px] font-black rounded border border-slate-300 uppercase tracking-tighter">离线</div>
      <div class="flex items-start gap-3">
        <div class="h-9 w-9 rounded-lg bg-slate-200 border border-slate-300 flex items-center justify-center text-slate-400 shrink-0">
          <span class="material-symbols-outlined text-xl">smartphone</span>
        </div>
        <div class="min-w-0 flex-1">
          <h3 class="text-[10px] font-bold text-slate-600 truncate pr-8">${esc(displayName)}</h3>
          <p class="mono-technical text-[9px] text-slate-400 mt-0.5 font-medium">${esc(shortHwid)}</p>
        </div>
      </div>
      <div class="mt-2.5 flex items-center justify-between text-[9px] font-bold">
        <div class="flex gap-3">
          <span class="flex items-center gap-1 text-slate-400">
            <span class="material-symbols-outlined icon-sm">battery_3_bar</span>${battery}%
          </span>
          <span class="flex items-center gap-1 text-slate-400">
            <span class="material-symbols-outlined icon-sm">device_thermostat</span>${temp}°C
          </span>
        </div>
        <span class="text-slate-400 uppercase text-[8px]">2小时前</span>
      </div>
    </div>`;
    };

    const emptyBody =
        '<div class="text-center text-[9px] text-slate-300 py-3 font-semibold uppercase tracking-wider">暂无设备</div>';
    let html = '';

    // Running section (always show)
    html += `<div class="dev-section">
      <div class="dev-section-header bg-[#eff6ff] px-3 py-1.5 flex items-center gap-2 sticky top-0 z-10 border-b border-blue-100" onclick="this.parentElement.classList.toggle('collapsed')">
        <span class="material-symbols-outlined section-arrow text-sm text-blue-400">expand_more</span>
        <span class="text-[10px] font-black text-slate-500 uppercase tracking-normal">运行中</span>
      </div>
      <div class="dev-section-body p-2 space-y-1">${runningDevs.length ? runningDevs.map(renderRunningCard).join('') : emptyBody}</div>
    </div>`;

    // Ready section (always show)
    html += `<div class="dev-section">
      <div class="dev-section-header bg-[#f8fafc] px-3 py-1.5 flex items-center gap-2 sticky top-0 z-10 border-b border-slate-100 mt-1" onclick="this.parentElement.classList.toggle('collapsed')">
        <span class="material-symbols-outlined section-arrow text-sm text-slate-400">expand_more</span>
        <span class="text-[10px] font-black text-slate-500 uppercase tracking-normal">就绪</span>
      </div>
      <div class="dev-section-body p-2 space-y-1">${readyDevs.length ? readyDevs.map(renderReadyCard).join('') : emptyBody}</div>
    </div>`;

    // Offline section (always show)
    html += `<div class="dev-section">
      <div class="dev-section-header bg-[#f8fafc] px-3 py-1.5 flex items-center gap-2 sticky top-0 z-10 border-b border-slate-100 mt-1" onclick="this.parentElement.classList.toggle('collapsed')">
        <span class="material-symbols-outlined section-arrow text-sm text-slate-400">expand_more</span>
        <span class="text-[10px] font-black text-slate-500 uppercase tracking-normal">离线</span>
      </div>
      <div class="dev-section-body p-2 space-y-1">${offlineDevs.length ? offlineDevs.map(renderOfflineCard).join('') : emptyBody}</div>
    </div>`;

    // Save collapsed state before re-render
    const collapsedSections = new Set<string>();
    tree.querySelectorAll('.dev-section.collapsed').forEach(el => {
        const label = el.querySelector('.dev-section-header span:last-child')?.textContent?.trim();
        if (label) collapsedSections.add(label);
    });

    tree.innerHTML = html;

    // Restore collapsed state
    tree.querySelectorAll('.dev-section').forEach(el => {
        const label = el.querySelector('.dev-section-header span:last-child')?.textContent?.trim();
        if (label && collapsedSections.has(label)) {
            el.classList.add('collapsed');
        }
    });

    // Bind events based on device state
    let clickTimer: ReturnType<typeof setTimeout> | null = null;
    tree.querySelectorAll('.dev-card').forEach(el => {
        const s = (el as HTMLElement).dataset.s!;
        const state = (el as HTMLElement).dataset.state;

        if (state === 'running') {
            // Running: 单击选中 + 切换任务，双击查看设备信息
            el.addEventListener('click', e => {
                e.stopPropagation();
                if (clickTimer) clearTimeout(clickTimer);
                clickTimer = setTimeout(() => selectDevice(s), 200);
            });
            el.addEventListener('dblclick', e => {
                e.stopPropagation();
                if (clickTimer) {
                    clearTimeout(clickTimer);
                    clickTimer = null;
                }
                showDeviceInfo(s);
            });
        } else if (state === 'offline') {
            // Offline: 单击选中（启用 Remove 按钮），双击查看设备信息
            el.addEventListener('click', e => {
                e.stopPropagation();
                if (clickTimer) clearTimeout(clickTimer);
                clickTimer = setTimeout(() => selectDevice(s), 200);
            });
            el.addEventListener('dblclick', e => {
                e.stopPropagation();
                if (clickTimer) {
                    clearTimeout(clickTimer);
                    clickTimer = null;
                }
                showDeviceInfo(s);
            });
        } else {
            // Ready: 只有双击查看设备信息
            el.addEventListener('dblclick', e => {
                e.stopPropagation();
                showDeviceInfo(s);
            });
        }
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
    // Find device executing this task
    const execDevice = task.assignedDevice ? task.assignedDevice.substring(0, 16) : '';

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
            <span class="px-1.5 py-0.5 bg-green-50 text-green-600 text-[8px] font-black rounded border border-green-100 uppercase tracking-tighter">运行中</span>
          </div>
          <div class="flex flex-col mt-0.5">
            <span class="text-[10px] font-bold text-blue-600 uppercase tracking-tight">正在执行设备: ${esc(execDevice)}</span>
            <div class="flex items-center text-[9px] text-slate-400 font-medium mt-0.5">
              <span class="material-symbols-outlined icon-xs text-[12px] mr-1">location_on</span>${task.cities.length} 个城市
              <span class="mx-2 text-slate-200">|</span>
              <span class="uppercase tracking-tighter">硬件同步已开启</span>
            </div>
          </div>
        </div>
      </div>
      <div class="flex items-center space-x-1.5">
        <button class="px-2.5 py-1.5 rounded-md border border-green-200 bg-green-50 text-[10px] font-black text-green-600 hover:bg-green-100 transition-all flex items-center gap-1.5 uppercase">
          <span class="material-symbols-outlined text-sm fill-1">play_arrow</span> 启动
        </button>
        <button class="px-2.5 py-1.5 rounded-md border border-amber-200 bg-amber-50 text-[10px] font-black text-amber-600 hover:bg-amber-100 transition-all flex items-center gap-1.5 uppercase">
          <span class="material-symbols-outlined text-sm">pause</span> 暂停
        </button>
        <button class="px-2.5 py-1.5 rounded-md border border-green-200 bg-green-50 text-[10px] font-black text-green-600 hover:bg-green-100 transition-all flex items-center gap-1.5 uppercase">
          <span class="material-symbols-outlined text-sm">resume</span> 继续
        </button>
        <button class="px-2.5 py-1.5 rounded-md border border-blue-200 bg-blue-50 text-[10px] font-black text-blue-600 hover:bg-blue-100 transition-all flex items-center gap-1.5 uppercase">
          <span class="material-symbols-outlined text-sm">replay</span> 重跑
        </button>
        <button class="px-2.5 py-1.5 rounded-md border border-red-200 bg-red-50 text-[10px] font-black text-red-600 hover:bg-red-100 transition-all flex items-center gap-1.5 uppercase">
          <span class="material-symbols-outlined text-sm">stop</span> 停止
        </button>
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
                      ? '<span class="text-[9px] text-slate-400 font-bold uppercase">已完成</span>'
                      : c.status === 'active'
                        ? `<span class="text-[9px] text-slate-400 font-bold uppercase">${c.done}/${c.total} 关键词</span>`
                        : '<span class="text-[9px] text-slate-400 font-bold uppercase">等待中</span>';
              const pctLabel =
                  c.status === 'done'
                      ? '<span class="text-[9px] font-black text-green-600">100%</span>'
                      : c.status === 'active'
                        ? `<span class="text-[9px] font-black text-blue-600">${c.progress}%</span>`
                        : '';
              return `
        <div class="bg-white rounded-md ${borderCls} p-2.5 cursor-pointer ${!isActive ? 'hover:bg-slate-50' : ''} transition-all relative overflow-hidden" style="width:180px;min-width:180px;flex-shrink:0" onclick="window.__switchCity(${i})">
          <div class="flex items-center justify-between mb-1.5">
            <div class="flex items-center space-x-2">
              <span class="material-symbols-outlined icon-sm text-blue-400">location_city</span>
              <span class="text-[11px] ${nameWeight}">${c.name}</span>
            </div>
            ${statusIcon}
          </div>
          <div class="text-[8px] text-slate-400 truncate mb-1.5" title="${c.poi}">
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
          <span class="text-[10px] font-black text-slate-500 uppercase tracking-tight">关键词: ${city.name}</span>
          <span class="px-1.5 py-0.5 bg-slate-200/50 text-slate-600 rounded text-[9px] font-black">共 ${city.total} 个</span>
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
                      return `<div class="kw-item flex items-center justify-between p-2 rounded-md bg-green-50/40 border border-green-100 transition-all hover:bg-green-50">
        <div class="flex items-center min-w-0">
          <span class="material-symbols-outlined icon-sm text-green-500 mr-1.5 fill-1">check_circle</span>
          <span class="text-[12px] font-semibold text-slate-700 truncate">${k.name}</span>
        </div>
      </div>`;
                  } else if (k.status === 'run') {
                      return `<div class="kw-item flex items-center p-2 rounded-md bg-blue-50 border border-blue-400 keyword-active ring-2 ring-blue-100">
        <div class="h-2 w-2 rounded-full bg-blue-500 mr-1.5 animate-pulse"></div>
        <span class="text-[12px] font-bold text-blue-700 truncate">${k.name}</span>
      </div>`;
                  } else {
                      return `<div class="kw-item flex items-center p-2 rounded-md bg-slate-50 border border-slate-100 hover:border-slate-300 transition-all cursor-pointer">
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
(window as any).__switchTask = (taskId: string) => {
    const task = globalQueue.find(t => t.id === taskId);
    if (!task || task === activeTask) return;
    activeTask = task;
    activeCityIdx = 0;
    renderTaskView();
    // Refresh queue to update active highlight
    if (selectedDevice) loadChainForDevice(selectedDevice);
};

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

(window as any).__filterKw = (val: string) => {
    const q = val.toLowerCase();
    document.querySelectorAll('#kw-grid .kw-item').forEach(el => {
        const name = el.textContent?.toLowerCase() || '';
        (el as HTMLElement).style.display = name.includes(q) ? '' : 'none';
    });
};

/* ───── Global Queue (right column, code.html style) ───── */
function loadChainForDevice(_serial: string) {
    const cards = $('#chain-cards')!;

    const queueSub = document.querySelector('.queue-sub');
    if (queueSub) queueSub.textContent = `${globalQueue.length} 个进行中任务`;

    cards.innerHTML = globalQueue
        .map(q => {
            const isActive = q === activeTask;
            const deviceSub = q.assignedDevice ? q.assignedDevice.substring(0, 14) : '';

            if (q.status === 'EXECUTING') {
                const kwTotal = q.cities.reduce((s, c) => s + c.total, 0);
                const kwDone = q.cities.reduce((s, c) => s + c.done, 0);
                const pct = kwTotal > 0 ? Math.round((kwDone / kwTotal) * 100) : 0;
                return `
      <div class="executing-gradient border border-blue-200 rounded-lg p-3 shadow-sm transition-all hover:shadow-md cursor-pointer relative overflow-hidden ring-1 ring-blue-50 ${isActive ? 'ring-2 ring-blue-300' : ''}" onclick="window.__switchTask('${q.id}')">
        <div class="flex justify-between items-start mb-1.5">
          <h4 class="text-[11px] font-bold text-slate-800 truncate pr-2">${q.name}</h4>
          <span class="px-1.5 py-0.5 bg-blue-500 text-white text-[7px] font-black rounded uppercase">执行中</span>
        </div>
        <div class="flex flex-col mb-2">
          <p class="mono-technical text-[8px] text-blue-600 font-bold uppercase mb-1">设备: ${esc(deviceSub)}</p>
          <div class="flex items-center text-[9px] text-slate-500">
            <span class="material-symbols-outlined icon-xs text-[14px] mr-1 text-blue-500">bolt</span> ${kwDone}/${kwTotal} 关键词
          </div>
        </div>
        <div class="w-full h-1 bg-blue-100 rounded-full overflow-hidden">
          <div class="h-full bg-blue-500 rounded-full" style="width: ${pct}%"></div>
        </div>
      </div>`;
            } else if (q.status === 'SCHEDULED') {
                const kwTotal = q.cities.reduce((s, c) => s + c.total, 0);
                return `
      <div class="bg-white border border-slate-100 border-l-4 border-l-slate-400 rounded-lg p-3 transition-all hover:border-slate-300 cursor-pointer" onclick="window.__switchTask('${q.id}')">
        <div class="flex justify-between items-start mb-1.5">
          <h4 class="text-[11px] font-bold text-slate-600 truncate pr-2">${q.name}</h4>
          <span class="px-1.5 py-0.5 bg-slate-100 text-slate-500 text-[7px] font-black rounded uppercase">已计划</span>
        </div>
        <p class="mono-technical text-[8px] text-slate-400 font-bold uppercase mb-1">设备: ${esc(deviceSub)}</p>
        <p class="text-[9px] text-slate-400 font-bold uppercase">${kwTotal} 个关键词 • 21:00</p>
      </div>`;
            } else if (q.status === 'SUCCESS') {
                return `
      <div class="bg-white border border-slate-100 border-l-4 border-l-green-500 rounded-lg p-3 opacity-80" onclick="window.__switchTask('${q.id}')">
        <div class="flex justify-between items-start mb-1.5">
          <h4 class="text-[11px] font-bold text-slate-500 truncate pr-2">${q.name}</h4>
          <span class="px-1.5 py-0.5 bg-green-50 text-green-600 text-[7px] font-black rounded uppercase">成功</span>
        </div>
        <p class="mono-technical text-[8px] text-slate-400 font-bold uppercase mb-1">设备: ${esc(deviceSub)}</p>
        <p class="text-[9px] text-slate-400 font-bold uppercase">已于 12:40 PM 完成</p>
      </div>`;
            } else if (q.status === 'ERROR') {
                return `
      <div class="bg-white border border-slate-100 border-l-4 border-l-red-500 rounded-lg p-3" onclick="window.__switchTask('${q.id}')">
        <div class="flex justify-between items-start mb-1.5">
          <h4 class="text-[11px] font-bold text-slate-700 truncate pr-2">${q.name}</h4>
          <span class="px-1.5 py-0.5 bg-red-50 text-red-500 text-[7px] font-black rounded uppercase">错误</span>
        </div>
        <p class="mono-technical text-[8px] text-slate-400 font-bold uppercase mb-1">设备: ${esc(deviceSub)}</p>
        <p class="text-[9px] text-red-500 font-bold uppercase">连接超时 (20s) - 自动重试中</p>
      </div>`;
            } else {
                // Default / PENDING
                return `
      <div class="bg-white border border-slate-100 border-l-4 border-l-slate-300 rounded-lg p-3 transition-all hover:border-slate-300 cursor-pointer" onclick="window.__switchTask('${q.id}')">
        <div class="flex justify-between items-start mb-1.5">
          <h4 class="text-[11px] font-bold text-slate-600 truncate pr-2">${q.name}</h4>
          <span class="px-1.5 py-0.5 bg-slate-100 text-slate-500 text-[7px] font-black rounded uppercase">${q.status}</span>
        </div>
        <p class="mono-technical text-[8px] text-slate-400 font-bold uppercase mb-1">设备: ${esc(deviceSub)}</p>
      </div>`;
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
    const d = document.createElement('div');
    d.textContent = t;
    return d.innerHTML;
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
    loadChainForDevice('');
    // Scan USB button
    $('#btn-scan-usb')?.addEventListener('click', async () => {
        const btn = $('#btn-scan-usb') as HTMLButtonElement;
        btn.disabled = true;
        try {
            await refreshDevices();
        } finally {
            btn.disabled = false;
        }
    });

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
        } catch (_) {
            /* ignore */
        }
        // Check MQTT status
        try {
            const s = await invoke<string>('mqtt_status');
            updateMqttStatusUI(s);
        } catch (_) {
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
    listen('devices-changed', () => {
        refreshDevices();
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
