import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { type MockTask, TASK_STATUS_STYLE, getTasksForDevice, MOCK_DEVICES, MOCK_DEVICE_PROPS } from "./mock-data";

/* ===== Types ===== */
interface DeviceInfo { serial: string; name: string; state: string; device_type: string }
interface DeviceProperties {
  serial: string; model: string; brand: string;
  android_version: string; sdk_version: string;
  display_resolution: string; device_type: string;
  battery_level: number; battery_temperature: number;
}

let selectedDevice: string | null = null;
let globalQueue: MockTask[] = [];
let activeTask: MockTask | null = null;
let activeCityIdx = 0;

const $ = (s: string) => document.querySelector(s) as HTMLElement | null;

/* ───── Splash ───── */
function splash() {
  const el = $("#splash")!;
  const app = $("#app")!;
  // Show current time
  const timeEl = el.querySelector(".splash-time");
  if (timeEl) {
    const now = new Date();
    const h = String(now.getHours()).padStart(2, '0');
    const m = String(now.getMinutes()).padStart(2, '0');
    timeEl.textContent = `${h}:${m}`;
  }
  // Trigger enhanced entrance animation
  setTimeout(() => el.classList.add("loaded"), 300);
  setTimeout(() => {
    el.classList.add("out");
    app.classList.add("show");
    setTimeout(() => el.remove(), 800);
  }, 3200);
}


/* ───── Device List ───── */
let devicePropsCache: Map<string, DeviceProperties> = new Map();
let lastDevices: DeviceInfo[] = [];
let lastPropsResults: (DeviceProperties | null)[] = [];

async function refreshDevices() {
  const tree = $("#device-tree")!;
  const status = $("#topbar-status");
  tree.innerHTML = '<div class="text-center p-5"><span class="spinner"></span></div>';

  try {
    let devs: DeviceInfo[];
    let propsResults: (DeviceProperties | null)[];

    try {
      devs = await invoke("list_devices");
    } catch {
      // 后端不可用时使用 mock 数据
      devs = MOCK_DEVICES as DeviceInfo[];
    }

    // 没有真实设备时使用 mock
    if (!devs.length) {
      devs = MOCK_DEVICES as DeviceInfo[];
    }

    const onlineCountText = $("#online-count-text");
    const onlineCount = devs.filter(d => d.state !== "Offline").length;

    if (onlineCountText) {
      onlineCountText.textContent = onlineCount > 0 ? `${onlineCount} Online` : "Online";
    }

    if (status) {
      if (onlineCount > 0) {
        status.textContent = "Connected";
        status.className = "topbar-connected";
      } else {
        status.textContent = devs.length > 0 ? "Offline" : "Waiting";
        status.className = "topbar-connected" + (devs.length > 0 ? " offline" : "");
      }
    }

    if (!devs.length) {
      renderDeviceCards([], []);
      return;
    }

    // 尝试从后端获取属性，失败时用 mock
    const propsPromises = devs.map(d => {
      if (d.state === "Offline") {
        const mockP = MOCK_DEVICE_PROPS.find(p => p.serial === d.serial);
        return Promise.resolve(mockP as DeviceProperties | null ?? null);
      }
      return invoke<DeviceProperties>("get_device_info", { serial: d.serial }).catch(() => {
        const mockP = MOCK_DEVICE_PROPS.find(p => p.serial === d.serial);
        return (mockP as DeviceProperties | null) ?? null;
      });
    });
    propsResults = await Promise.all(propsPromises);

    devicePropsCache.clear();
    propsResults.forEach((p, i) => { if (p) devicePropsCache.set(devs[i].serial, p); });

    // Cache for re-render on selection
    lastDevices = devs;
    lastPropsResults = propsResults;

    renderDeviceCards(devs, propsResults);

    // 默认选中第一台在线设备
    if (!selectedDevice) {
      const firstOnline = devs.find(d => d.state !== "Offline");
      if (firstOnline) selectDevice(firstOnline.serial);
    }
  } catch (e) {
    tree.innerHTML = `<div class="empty-hint text-red">Error: ${e}</div>`;
    if (status) {
      status.textContent = "Error";
      status.className = "topbar-connected offline";
    }
  }
}

function renderDeviceCards(devs: DeviceInfo[], propsResults: (DeviceProperties | null)[]) {
  const tree = $("#device-tree")!;
  const countText = $("#online-count-text");
  if (countText) countText.textContent = `${devs.length} Total`;

  // Split devices: selected + running task → Active, rest → Connected & Offline
  const activeDevs: { d: DeviceInfo; p: DeviceProperties | null; idx: number }[] = [];
  const idleDevs: { d: DeviceInfo; p: DeviceProperties | null; idx: number }[] = [];

  // Active = 在线设备 + 有正在执行的任务绑定
  const executingSerials = new Set(
    globalQueue.filter(t => t.status === 'EXECUTING' && t.assignedDevice).map(t => t.assignedDevice!)
  );

  devs.forEach((d, idx) => {
    const p = propsResults[idx];
    if (d.state !== "Offline" && executingSerials.has(d.serial)) {
      activeDevs.push({ d, p, idx });
    } else {
      idleDevs.push({ d, p, idx });
    }
  });




  const renderActiveCard = (item: { d: DeviceInfo; p: DeviceProperties | null }) => {
    const { d, p } = item;
    const displayName = p ? `${p.brand} ${p.model}` : d.name || d.serial;
    const battery = p?.battery_level ?? 0;
    const temp = p?.battery_temperature ?? 0;
    const shortHwid = d.serial.length > 16 ? d.serial.substring(0, 16) + '…' : d.serial;
    // 查找该设备绑定的任务
    const devTask = globalQueue.find(t => t.assignedDevice === d.serial);
    const devCity = devTask?.cities.find(c => c.status === 'active') ?? devTask?.cities[0];
    const taskLabel = devTask ? devTask.name : 'Idle';
    const cityLabel = devCity?.name || '';
    const progress = devCity?.progress ?? 0;

    return `<div class="dev-card flex flex-col gap-0 p-2.5 px-3 mb-1 rounded-sm cursor-pointer border-1 border-green/20 transition-all duration-[180ms] relative bg-gradient-to-br from-green/[.08] to-green/[.18] shadow-[0_4px_20px_rgba(16,185,129,.18),0_0_0_1px_rgba(16,185,129,.1)]" data-s="${esc(d.serial)}">
      <div class="flex items-center gap-3">
        <div class="w-[42px] h-[42px] rounded-md bg-green/[.1] border-[1.5px] border-green/25 flex items-center justify-center shrink-0 text-green">
          <span class="material-icons-round text-[22px]">smartphone</span>
        </div>
        <div class="flex-1 min-w-0">
          <div class="text-[13px] font-bold text-s900 truncate">${esc(displayName)}</div>
          <div class="text-[11px] text-gray-500 mt-0.5 truncate font-mono">HWID: ${esc(shortHwid)}</div>
        </div>
        <span class="text-[10px] font-bold uppercase tracking-wide px-2 py-0.5 rounded-sm leading-snug text-green bg-green/[.08] border border-green/15">RUNNING</span>
      </div>
      <div class="flex justify-between items-center mt-2.5 px-0.5">
        <span class="text-[13px] font-semibold text-s600">Task: ${esc(taskLabel)}</span>
        <span class="text-xs font-semibold text-blue">${progress}%</span>
      </div>
      <div class="h-1.5 bg-blue/15 rounded-sm mt-1.5 overflow-hidden"><div class="h-full bg-blue rounded-sm transition-[width] duration-300" style="width:${progress}%"></div></div>
      <div class="flex items-center gap-3 mt-2.5 px-0.5">
        <span class="text-xs font-semibold text-s500 inline-flex items-center gap-0.5"><span class="material-icons-round text-[16px] scale-[.65] text-green">battery_charging_full</span> ${battery}% </span>
        <span class="text-xs font-semibold text-s500 inline-flex items-center gap-0.5"><span class="material-icons-round text-[16px] scale-[.65] text-orange">thermostat</span> ${temp}°C</span>
        <span class="ml-auto text-xs font-bold text-green uppercase tracking-wide">${esc(cityLabel)}</span>
      </div>
    </div>`;
  };

  const renderIdleCard = (item: { d: DeviceInfo; p: DeviceProperties | null }) => {
    const { d, p } = item;
    const on = d.state !== "Offline";
    const displayName = p ? `${p.brand} ${p.model}` : d.name || d.serial;
    const battery = p?.battery_level ?? 0;
    const temp = p?.battery_temperature ?? 0;
    const isSel = d.serial === selectedDevice;
    const shortSn = d.serial.length > 14 ? d.serial.substring(0, 14) : d.serial;
    const statusBadge = on
      ? '<span class="text-[10px] font-bold uppercase tracking-wide px-2 py-0.5 rounded-sm leading-snug text-blue bg-blue/[.08] border border-blue/15">READY</span>'
      : '<span class="text-[10px] font-bold uppercase tracking-wide px-2 py-0.5 rounded-sm leading-snug text-s400 bg-s50 border border-s200">OFFLINE</span>';
    const batteryColor = !on ? 'color:var(--color-red)' : battery < 30 ? 'color:var(--color-orange)' : 'color:var(--color-green)';
    const tempColor = temp > 40 ? 'color:var(--color-orange)' : '';
    const bottomRight = on
      ? `<span class="text-base text-s400">⌁</span>`
      : '<span class="text-[11px] font-semibold text-s400 tracking-wide">2H AGO</span>';

    return `<div class="dev-card flex flex-col gap-0 p-2.5 px-3 mb-1 rounded-sm cursor-pointer border-1 ${isSel && !on ? 'border-blue/15 bg-gradient-to-br from-s200 to-s300 shadow-[0_6px_24px_rgba(71,85,105,.25),0_0_0_2px_rgba(71,85,105,.15)] -translate-y-px' : isSel ? 'border-blue/20 bg-gradient-to-br from-blue/[.06] to-blue/[.14] shadow-[0_4px_20px_rgba(37,99,235,.18),0_0_0_1px_rgba(37,99,235,.1)]' : !on ? 'border-s200 bg-white/60' : 'border-blue/20 bg-gradient-to-br from-blue/[.08] to-blue/[.16] shadow-[0_4px_20px_rgba(37,99,235,.15),0_0_0_1px_rgba(37,99,235,.08)]'} transition-all duration-[180ms] relative" data-s="${esc(d.serial)}">
      <div class="flex items-center gap-3">
        <div class="w-[42px] h-[42px] rounded-md bg-s50 border-[1.5px] border-s200 flex items-center justify-center shrink-0 text-s400">
          <span class="material-icons-round text-[22px]">smartphone</span>
        </div>
        <div class="flex-1 min-w-0">
          <div class="text-[13px] font-bold text-s900 truncate">${esc(displayName)}</div>
          <div class="text-[11px] text-gray-500 mt-0.5 truncate font-mono">HWID: ${esc(shortSn)}</div>
        </div>
        ${statusBadge}
      </div>
      <div class="flex items-center gap-2.5 mt-2.5 px-0.5">
        <span class="text-xs font-semibold text-s500 inline-flex items-center gap-0.5" style="${batteryColor}"><span class="material-icons-round text-[16px] scale-[.65] text-green">battery_charging_full</span> ${battery}%</span>
        <span class="text-xs font-semibold text-s500 inline-flex items-center gap-0.5" style="${tempColor}"><span class="material-icons-round text-[16px] scale-[.65] text-orange">thermostat</span> ${temp}°C</span>
        <span class="ml-auto">${bottomRight}</span>
      </div>
    </div>`;
  };

  let html = '';

  // Active Automation section
  const chevron = `<svg class="section-chevron inline-block transition-transform duration-200 align-[-1px] mr-0.5" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="6 9 12 15 18 9"/></svg>`;

  html += `<div class="dev-section mb-1">
    <div class="flex justify-between items-center px-3.5 pt-2.5 pb-1.5 text-[10px] font-extrabold text-s400 uppercase tracking-[1.5px] cursor-pointer select-none hover:text-s600 font-sans" onclick="this.parentElement.classList.toggle('collapsed')">
      <span>${chevron} ACTIVE AUTOMATION</span>
    </div>
    <div class="dev-section-body px-1">
      ${activeDevs.length ? activeDevs.map(renderActiveCard).join('') : '<div class="text-xs text-s300 text-center py-4">No active tasks</div>'}
    </div>
  </div>`;

  // Connected & Offline section
  html += `<div class="dev-section mb-1">
    <div class="flex justify-between items-center px-3.5 pt-2.5 pb-1.5 text-[10px] font-extrabold text-s400 uppercase tracking-[1.5px] cursor-pointer select-none hover:text-s600" onclick="this.parentElement.classList.toggle('collapsed')">
      <span>${chevron} CONNECTED & OFFLINE</span>
    </div>
    <div class="dev-section-body px-1">
      ${idleDevs.length ? idleDevs.map(renderIdleCard).join('') : '<div class="text-xs text-s300 text-center py-4">No other devices</div>'}
    </div>
  </div>`;

  tree.innerHTML = html;

  // All cards are clickable for selection
  tree.querySelectorAll(".dev-card").forEach(el => {
    const s = (el as HTMLElement).dataset.s!;
    el.addEventListener("click", () => selectDevice(s));
    el.addEventListener("dblclick", () => showDeviceInfo(s));
  });
}


// Search filter: show/hide device cards based on query
function filterDeviceCards(query: string) {
  const q = query.toLowerCase().trim();
  document.querySelectorAll(".dev-card").forEach(el => {
    const name = el.querySelector(".dev-name")?.textContent?.toLowerCase() || "";
    const sub = el.querySelector(".dev-sub")?.textContent?.toLowerCase() || "";
    const match = !q || name.includes(q) || sub.includes(q);
    (el as HTMLElement).style.display = match ? "" : "none";
  });
}

function selectDevice(serial: string) {
  // Toggle: clicking the already-selected card deselects it
  if (selectedDevice === serial) {
    selectedDevice = null;
    if (lastDevices.length) {
      renderDeviceCards(lastDevices, lastPropsResults);
    }
    const removeBtn = $("#btn-remove-selected") as HTMLButtonElement;
    if (removeBtn) removeBtn.disabled = true;
    // Reset middle pane
    const colMid = $("#col-mid")!;
    colMid.innerHTML = '<div class="empty-hint" style="padding:40px;text-align:center">Select a device to view tasks</div>';
    return;
  }

  selectedDevice = serial;

  // Re-render device cards to update selection visual
  if (lastDevices.length) {
    renderDeviceCards(lastDevices, lastPropsResults);
  }

  // Enable Remove button only for offline devices
  const removeBtn = $("#btn-remove-selected") as HTMLButtonElement;
  if (removeBtn) {
    const card = document.querySelector(`.dev-card[data-s="${serial}"]`);
    const isDeviceOffline = Array.from(card?.querySelectorAll('span') || []).some(
      s => s.textContent?.trim() === 'OFFLINE'
    );
    removeBtn.disabled = !isDeviceOffline;
  }

  loadTasksForDevice(serial);
}

/* ───── Add Device Dialog ───── */
function showAddDeviceDialog() {
  const modal = $("#add-device-modal")!;
  const addrInput = $("#add-device-addr") as HTMLInputElement;
  const nameInput = $("#add-device-name") as HTMLInputElement;
  const errEl = $("#add-device-error")!;

  addrInput.value = "";
  nameInput.value = "";
  errEl.style.display = "none";
  modal.style.display = "flex";
  setTimeout(() => addrInput.focus(), 100);
}

function hideAddDeviceDialog() {
  const modal = $("#add-device-modal")!;
  modal.style.display = "none";
}

async function submitAddDevice() {
  const addrInput = $("#add-device-addr") as HTMLInputElement;
  const nameInput = $("#add-device-name") as HTMLInputElement;
  const errEl = $("#add-device-error")!;
  const submitBtn = $("#add-device-submit") as HTMLButtonElement;

  const address = addrInput.value.trim();
  const name = nameInput.value.trim();

  if (!address) {
    errEl.textContent = "请输入设备 IP 地址";
    errEl.style.display = "block";
    return;
  }

  // Phase 1: 按钮进入 loading 态
  submitBtn.disabled = true;
  submitBtn.innerHTML = '<span class="btn-spinner"></span> Connecting…';
  submitBtn.classList.add("connecting");
  errEl.style.display = "none";

  // 等一帧，让浏览器先渲染 spinner 动画
  await nextFrame();

  try {
    await invoke("add_device", { address, name });

    // Phase 2: 关闭对话框，设备列表显示"正在连接"的卡片
    hideAddDeviceDialog();
    const tree = $("#device-tree")!;
    const displayName = name || address;
    const initial = displayName.charAt(0).toUpperCase();
    tree.innerHTML = `<div class="dev-card connecting-card">
      <div class="dev-avatar connecting-avatar">
        <span class="dev-initial">${initial}</span>
        <div class="avatar-spinner"></div>
      </div>
      <div class="dev-info">
        <div class="dev-name">${esc(displayName)}</div>
        <div class="dev-sub">${esc(address)} · <span class="connecting-text">Connecting…</span></div>
      </div>
    </div>`;

    // 等一帧，让浏览器先渲染连接卡片
    await nextFrame();

    // Phase 3: 刷新设备列表（此步骤含 TCP 超时检测，耗时）
    await refreshDevices();
  } catch (e) {
    errEl.textContent = `${e}`;
    errEl.style.display = "block";
  } finally {
    submitBtn.disabled = false;
    submitBtn.innerHTML = "Connect";
    submitBtn.classList.remove("connecting");
  }
}

async function removeSelectedDevice() {
  if (!selectedDevice) return;
  // Only allow removing offline devices
  const card = document.querySelector(`.dev-card[data-s="${selectedDevice}"]`);
  const isOffline = Array.from(card?.querySelectorAll('span') || []).some(
    s => s.textContent?.trim() === 'OFFLINE'
  );
  if (!isOffline) return;
  try {
    await invoke("remove_device", { serial: selectedDevice });
    selectedDevice = null;
    const removeBtn = $("#btn-remove-selected") as HTMLButtonElement;
    if (removeBtn) removeBtn.disabled = true;
    await refreshDevices();
  } catch (e) {
    console.error(`Remove failed: ${e}`);
  }
}


/* ───── Task View (middle column) ───── */
function loadTasksForDevice(serial: string) {
  // 模拟根据 HWID 从服务端拉取任务列表
  const deviceTasks = getTasksForDevice(serial);
  // 合并到全局队列（去重）
  const seen = new Set(globalQueue.map(t => t.id));
  for (const t of deviceTasks) {
    if (!seen.has(t.id)) {
      globalQueue.push(t);
      seen.add(t.id);
    }
  }
  // 默认选中第一个 EXECUTING 任务，否则选第一个
  activeTask = globalQueue.find(t => t.status === 'EXECUTING') ?? globalQueue[0] ?? null;
  activeCityIdx = 0;
  renderTaskView();
  loadChainForDevice(serial);
}

function renderTaskView() {
  const mid = document.getElementById("col-mid")!;
  const task = activeTask;
  if (!task) {
    mid.innerHTML = '<div class="empty-hint">Select a task from the queue</div>';
    return;
  }
  const city = task.cities[activeCityIdx];
  const cityPinSvg = '<svg class="shrink-0 text-s400" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 10c0 7-9 13-9 13s-9-6-9-13a9 9 0 0 1 18 0z"/><circle cx="12" cy="10" r="3"/></svg>';

  mid.innerHTML = `
    <!-- Task Header -->
    <div class="flex items-center justify-between px-5 py-4 gap-3 border-b border-s100 shrink-0 rounded-t-xl">
      <div class="flex items-center gap-4">
        <div class="w-[52px] h-[52px] rounded-xl bg-gradient-to-br from-[#6366f1] to-[#4f46e5] flex items-center justify-center shrink-0 shadow-[0_4px_16px_rgba(99,102,241,.35)]">
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="8" r="2"/><circle cx="6" cy="16" r="2"/><circle cx="18" cy="16" r="2"/><path d="M12 10v2l-4 3"/><path d="M12 12l4 3"/></svg>
        </div>
        <div>
          <h3 class="text-xl font-extrabold text-s900 m-0 leading-snug flex items-center gap-2.5 font-sans">
            Task: <span class="text-s900 font-extrabold">${task.name}</span>
            <span class="text-[10px] font-bold px-2.5 py-0.5 rounded-full border-1 border-green text-green tracking-wider uppercase">LIVE</span>
          </h3>
          <span class="text-[11px] text-s400 font-semibold uppercase tracking-[1.5px] mt-0.5 block">📍 ${task.cities.length} Cities &nbsp;|&nbsp; Hardware Monitoring Active</span>
        </div>
      </div>
      <div class="flex gap-2 shrink-0">
        <button class="inline-flex items-center gap-1.5 px-4 py-2 border-none rounded-lg bg-s100 text-s600 font-sans text-xs font-semibold cursor-pointer transition-all duration-150 hover:bg-s200 hover:text-s800">⏸ Pause</button>
        <button class="inline-flex items-center gap-1.5 px-4 py-2 rounded-lg text-red text-xs font-semibold cursor-pointer transition-all duration-150 border border-red bg-red-light hover:bg-red/20">⏹ Stop</button>
      </div>
    </div>

    <!-- City Tabs -->
    <div class="flex gap-2.5 px-5 py-4 overflow-x-auto shrink-0 border-b border-s100">
      ${task.cities.map((c, i) => {
    const isActive = i === activeCityIdx;
    const icon = cityPinSvg;
    const statusEl = c.status === 'done'
      ? '<span class="w-5 h-5 rounded-full bg-green text-white inline-flex items-center justify-center text-[11px] font-bold shrink-0">✓</span>'
      : c.status === 'active'
        ? '<span class="w-3 h-3 rounded-full bg-blue shrink-0"></span>'
        : '<svg class="shrink-0 text-s300" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>';
    const progressBar = c.status === 'active'
      ? `<div class="h-[4px] bg-s100 rounded-full overflow-hidden mt-2 mb-1.5"><div class="h-full bg-blue rounded-full transition-[width] duration-300" style="width:${c.progress}%"></div></div>`
      : c.status === 'done'
        ? `<div class="h-[4px] bg-green/20 rounded-full overflow-hidden mt-2 mb-1.5"><div class="h-full bg-green rounded-full" style="width:100%"></div></div>`
        : '';
    const statsText = c.status === 'active'
      ? `<div class="flex justify-between text-[11px] font-semibold"><span class="text-s500">${c.done}/${c.total} KWS</span><span class="text-blue font-bold">${c.progress}%</span></div>`
      : c.status === 'done'
        ? `<div class="flex justify-between text-[11px] font-semibold"><span class="text-s400 uppercase tracking-wide">Done</span><span class="text-green font-bold">${c.progress}%</span></div>`
        : `<div class="text-[11px] font-semibold text-s400 uppercase tracking-wide mt-2">Pending</div>`;
    return `
        <div class="flex-1 min-w-[160px] px-3.5 py-2.5 border-2 ${isActive ? 'border-blue bg-white shadow-[0_2px_12px_rgba(37,99,235,.1)]' : 'border-s200/60 bg-white/80'} rounded-sm cursor-pointer transition-all duration-[180ms] shrink-0 hover:border-s300 font-sans" onclick="window.__switchCity(${i})">
          <div class="flex items-center justify-between mb-0.5">
            <div class="flex items-center gap-1.5">${icon}<span class="text-[14px] font-bold ${isActive ? 'text-s900' : 'text-s700'}">${c.name}</span></div>
            ${statusEl}
          </div>
          ${progressBar}
          ${statsText}
        </div>`;
  }).join('')}
    </div>

    <!-- Keywords Section -->
    <div class="flex-1 overflow-y-auto px-5 py-4">
      <div class="flex items-center gap-3 mb-5">
        <span class="text-xs font-extrabold text-s500 uppercase tracking-[2px] font-sans">Keywords: ${city.name}</span>
        <span class="inline-flex items-center justify-center px-2.5 py-0.5 rounded-md text-[11px] font-bold bg-s100 text-s500 border border-s200">${city.total}</span>
        <div class="ml-auto">
          <input type="text" placeholder="Filter..." class="px-3 py-1.5 border border-s200 rounded-lg text-xs font-sans text-s600 bg-white outline-none w-[140px] transition-colors duration-150 focus:border-blue placeholder:text-s300" id="kw-filter-input" oninput="window.__filterKw(this.value)" />
        </div>
      </div>
      <div class="grid grid-cols-3 gap-2.5" id="kw-grid">
        ${city.keywords.map(k => {
    const dotEl = k.status === 'ok'
      ? '<span class="w-5 h-5 rounded-full bg-green text-white inline-flex items-center justify-center shrink-0 text-[11px] font-bold">✓</span>'
      : k.status === 'run'
        ? '<span class="w-5 h-5 rounded-full bg-blue text-white inline-flex items-center justify-center shrink-0 text-[8px]">●</span>'
        : '<span class="w-5 h-5 rounded-full bg-s200/60 text-s400 inline-flex items-center justify-center shrink-0 text-[9px] tracking-tighter">···</span>';
    const itemCls = k.status === 'ok' ? 'bg-green/[.06] border-green/20'
      : k.status === 'run' ? 'bg-blue/[.06] border-blue/20'
        : 'bg-white border-s100';
    const textCls = k.status === 'ok' ? 'text-s800 font-semibold'
      : k.status === 'run' ? 'text-blue font-semibold'
        : 'text-s500 font-medium';
    return `<div class="flex items-center gap-2 px-3 py-2.5 border rounded-lg text-[13px] ${textCls} transition-all duration-150 hover:shadow-1 ${itemCls}">${dotEl} ${k.name}</div>`;
  }).join('')}
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
};

(window as any).__filterKw = (val: string) => {
  const q = val.toLowerCase();
  document.querySelectorAll("#kw-grid .kw-item").forEach(el => {
    const name = el.textContent?.toLowerCase() || "";
    (el as HTMLElement).style.display = name.includes(q) ? "" : "none";
  });
};


/* ───── Global Queue (right column) ───── */
function loadChainForDevice(_serial: string) {
  const cards = $("#chain-cards")!;

  // Update queue count
  const queueSub = document.querySelector(".queue-sub");
  if (queueSub) queueSub.textContent = `${globalQueue.length} Active Tasks`;

  cards.innerHTML = globalQueue.map(q => {
    const style = TASK_STATUS_STYLE[q.status];
    const isActive = q === activeTask;
    const sub = q.assignedDevice ? `⚡ ${q.assignedDevice.substring(0, 12)}` : `${q.cities.reduce((s, c) => s + c.total, 0)} Keywords`;
    return `
    <div class="rounded-lg overflow-hidden bg-white border border-s100 border-l-4 shadow-1 transition-all duration-[180ms] cursor-pointer shrink-0 hover:shadow-2 hover:-translate-y-px ${isActive ? 'border-l-[5px] !bg-blue/[.06] shadow-2' : ''}" style="border-left-color:${style.color};background:${isActive ? 'rgba(37,99,235,.08)' : style.bg}" onclick="window.__switchTask('${q.id}')">
      <div class="px-3.5 py-3 flex items-center justify-between gap-3 min-h-12">
        <div>
          <h4 class="text-[13px] font-bold text-s900 m-0 font-sans leading-snug">${q.name}</h4>
          <small class="text-[10px] font-semibold uppercase tracking-wide text-s400 mt-0.5 block">${sub}</small>
        </div>
        <div class="text-right flex flex-col items-end gap-1 shrink-0">
          <span class="text-base leading-none">${style.icon}</span>
          <div class="text-[10px] font-extrabold tracking-wider uppercase mt-0.5" style="color:${style.color}">${q.status}</div>
        </div>
      </div>
    </div>`;
  }).join("") + '<div class="p-5 text-center text-[11px] font-bold uppercase tracking-[2px] text-s300">End of Queue</div>';
}

/* ───── Device Info Modal ───── */
async function showDeviceInfo(serial: string) {
  const m = $("#modal")!;
  const b = $("#modal-body")!;
  m.style.display = "flex";
  b.innerHTML = '<div class="text-center p-4.5"><span class="spinner"></span></div>';
  try {
    let i: DeviceProperties;
    try {
      i = await invoke("get_device_info", { serial });
    } catch {
      const mockP = MOCK_DEVICE_PROPS.find(p => p.serial === serial);
      if (!mockP) throw new Error('Device not found');
      i = mockP as DeviceProperties;
    }
    const typeLabel = i.device_type === "usb" ? "USB" : "WiFi";
    const typeIcon = i.device_type === "usb"
      ? '<span class="material-icons-round text-base">usb</span>'
      : '<span class="material-icons-round text-base">wifi</span>';
    const batteryPct = i.battery_level ?? 0;
    const batteryColor = batteryPct < 30 ? 'text-orange' : 'text-green';
    const tempVal = i.battery_temperature ?? 0;
    const tempColor = tempVal > 40 ? 'text-orange' : 'text-s500';

    b.innerHTML = `
      <!-- Device Header -->
      <div class="flex items-center gap-3 mb-5">
        <div class="w-12 h-12 rounded-lg bg-gradient-to-br from-blue to-blue-dark flex items-center justify-center shrink-0 shadow-[0_4px_12px_rgba(37,99,235,.3)]">
          <span class="material-icons-round text-2xl text-white">smartphone</span>
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
          <div class="text-[10px] font-bold text-s400 uppercase tracking-wider mb-1">Battery</div>
          <div class="text-[16px] font-bold ${batteryColor}">${batteryPct}%</div>
        </div>
        <div class="flex-1 px-3 py-2.5 rounded-lg bg-s50 border border-s100 text-center">
          <div class="text-[10px] font-bold text-s400 uppercase tracking-wider mb-1">Temp</div>
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
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">Serial</span>
          <span class="text-[12px] font-mono font-medium text-s700">${esc(i.serial)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-s50 border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">Brand</span>
          <span class="text-[12px] font-medium text-s700">${esc(i.brand)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-white border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">Model</span>
          <span class="text-[12px] font-medium text-s700">${esc(i.model)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-s50 border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">SDK Level</span>
          <span class="text-[12px] font-mono font-medium text-s700">${esc(i.sdk_version)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-white">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">Resolution</span>
          <span class="text-[12px] font-mono font-medium text-s700">${esc(i.display_resolution)}</span>
        </div>
      </div>`;
  } catch (e) {
    b.innerHTML = `<p class="text-red text-center py-4">Failed: ${e}</p>`;
  }
}



/* ───── Utility ───── */
function esc(t: string) {
  const d = document.createElement("div");
  d.textContent = t;
  return d.innerHTML;
}

/** 等待浏览器完成一帧渲染（双 rAF 保证 paint 完成） */
function nextFrame(): Promise<void> {
  return new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r())));
}

/* ───── Init ───── */
window.addEventListener("DOMContentLoaded", () => {
  splash();

  // 初始化全局任务队列
  globalQueue = getTasksForDevice("*");
  loadChainForDevice("");
  // Scan USB button
  $("#btn-scan-usb")?.addEventListener("click", async () => {
    const btn = $("#btn-scan-usb") as HTMLButtonElement;
    btn.disabled = true;
    try {
      await refreshDevices();
    } finally {
      btn.disabled = false;
    }
  });

  // Search toggle
  $("#btn-search-dev")?.addEventListener("click", () => {
    const box = $("#dev-search");
    const searchInp = $("#dev-search-input") as HTMLInputElement | null;
    if (!box || !searchInp) return;
    const visible = box.style.display !== "none";
    box.style.display = visible ? "none" : "block";
    if (!visible) { searchInp.value = ""; searchInp.focus(); }
    else filterDeviceCards("");
  });

  $("#dev-search-input")?.addEventListener("input", (e) => {
    filterDeviceCards((e.target as HTMLInputElement).value);
  });

  // Add Device (WiFi)
  $("#btn-add-device")?.addEventListener("click", showAddDeviceDialog);
  $("#add-device-close")?.addEventListener("click", hideAddDeviceDialog);
  $("#add-device-cancel")?.addEventListener("click", hideAddDeviceDialog);
  $("#add-device-submit")?.addEventListener("click", submitAddDevice);
  $("#add-device-modal")?.addEventListener("click", e => {
    if (e.target === e.currentTarget) hideAddDeviceDialog();
  });
  // Enter key to submit
  $("#add-device-addr")?.addEventListener("keydown", e => {
    if ((e as KeyboardEvent).key === "Enter") submitAddDevice();
  });
  $("#add-device-name")?.addEventListener("keydown", e => {
    if ((e as KeyboardEvent).key === "Enter") submitAddDevice();
  });

  // Remove Selected
  $("#btn-remove-selected")?.addEventListener("click", removeSelectedDevice);



  // Modal
  $("#modal-close")?.addEventListener("click", () => {
    ($("#modal") as HTMLElement).style.display = "none";
  });
  $("#modal")?.addEventListener("click", e => {
    if (e.target === e.currentTarget) (e.currentTarget as HTMLElement).style.display = "none";
  });

  // ── Settings ──
  const settingsOverlay = $("#settings-overlay") as HTMLElement;

  $("#btn-settings")?.addEventListener("click", async () => {
    // Load settings from backend
    try {
      const settings = await invoke<Record<string, string>>("get_settings");
      ($("#set-mqtt-host") as HTMLInputElement).value = settings.mqtt_host || "";
      ($("#set-mqtt-port") as HTMLInputElement).value = settings.mqtt_port || "1883";
      ($("#set-mqtt-client-id") as HTMLInputElement).value = settings.mqtt_client_id || "";
      ($("#set-mqtt-username") as HTMLInputElement).value = settings.mqtt_username || "";
      ($("#set-mqtt-password") as HTMLInputElement).value = settings.mqtt_password || "";
    } catch (_) { /* ignore */ }
    // Check MQTT status
    try {
      const s = await invoke<string>("mqtt_status");
      updateMqttStatusUI(s);
    } catch (_) { /* ignore */ }
    settingsOverlay.style.display = "flex";
  });

  $("#settings-close")?.addEventListener("click", () => settingsOverlay.style.display = "none");
  $("#settings-cancel")?.addEventListener("click", () => settingsOverlay.style.display = "none");
  settingsOverlay?.addEventListener("click", e => {
    if (e.target === e.currentTarget) settingsOverlay.style.display = "none";
  });

  $("#settings-save")?.addEventListener("click", async () => {
    const settings = {
      mqtt_host: ($("#set-mqtt-host") as HTMLInputElement).value,
      mqtt_port: ($("#set-mqtt-port") as HTMLInputElement).value,
      mqtt_client_id: ($("#set-mqtt-client-id") as HTMLInputElement).value,
      mqtt_username: ($("#set-mqtt-username") as HTMLInputElement).value,
      mqtt_password: ($("#set-mqtt-password") as HTMLInputElement).value,
    };
    try {
      await invoke("save_settings", { settings });
      settingsOverlay.style.display = "none";
    } catch (e) {
      console.error("Save settings failed:", e);
    }
  });

  $("#btn-mqtt-connect")?.addEventListener("click", async () => {
    try {
      await invoke("mqtt_connect");
      updateMqttStatusUI("connecting");
    } catch (e) {
      updateMqttStatusUI(`error:${e}`);
    }
  });

  $("#btn-mqtt-disconnect")?.addEventListener("click", async () => {
    try {
      await invoke("mqtt_disconnect");
      updateMqttStatusUI("disconnected");
    } catch (e) {
      console.error("MQTT disconnect failed:", e);
    }
  });

  // ── Panel Resizers ──
  function initResizer(resizerId: string, targetCol: string, side: "left" | "right") {
    const resizer = document.getElementById(resizerId);
    const col = document.querySelector(targetCol) as HTMLElement;
    if (!resizer || !col) return;

    let startX = 0;
    let startW = 0;

    const onMouseMove = (e: MouseEvent) => {
      e.preventDefault();
      const dx = e.clientX - startX;
      const newW = side === "left" ? startW + dx : startW - dx;
      const clamped = Math.max(200, Math.min(500, newW));
      col.style.width = `${clamped}px`;
    };

    const onMouseUp = () => {
      resizer.classList.remove("dragging");
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    };

    resizer.addEventListener("mousedown", (e: MouseEvent) => {
      e.preventDefault();
      startX = e.clientX;
      startW = col.getBoundingClientRect().width;
      resizer.classList.add("dragging");
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      document.addEventListener("mousemove", onMouseMove);
      document.addEventListener("mouseup", onMouseUp);
    });
  }

  initResizer("resizer-left", "#col-left", "left");
  initResizer("resizer-right", "#col-right", "right");

  // 监听后台 ADB 设备变更事件，自动刷新
  listen("devices-changed", () => {
    refreshDevices();
  });

  // 监听 MQTT 状态事件
  listen<string>("mqtt-status", (event) => {
    updateMqttStatusUI(event.payload);
  });

  // Auto-refresh after splash
  setTimeout(refreshDevices, 2400);
});

function updateMqttStatusUI(status: string) {
  const dot = $("#mqtt-dot");
  const text = $("#mqtt-status-text");
  const btnConn = $("#btn-mqtt-connect") as HTMLButtonElement;
  const btnDisc = $("#btn-mqtt-disconnect") as HTMLButtonElement;

  if (dot) {
    dot.className = "mqtt-dot";
    if (status === "connected") dot.classList.add("connected");
    else if (status.startsWith("error")) dot.classList.add("error");
  }
  if (text) {
    if (status === "connected") text.textContent = "Connected";
    else if (status === "connecting") text.textContent = "Connecting...";
    else if (status === "disconnected") text.textContent = "Disconnected";
    else if (status.startsWith("error:")) text.textContent = `Error: ${status.slice(6)}`;
    else text.textContent = status;
  }
  if (btnConn) btnConn.disabled = status === "connected" || status === "connecting";
  if (btnDisc) btnDisc.disabled = status !== "connected";
}
